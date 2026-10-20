//! Complete renderer-boundary to atomic-dataset acceptance test.

use std::{collections::BTreeMap, fs, num::NonZeroU32, sync::Arc};

use bop_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh, VisualPrimitive,
};
use bop_core::{
    AssetId, BackendInfo, BackendKind, NonNegativeFinite, RenderChannels, RenderError,
    RenderReport, RenderSceneRequest, RenderStageTimings, RendererCapabilities, RunConfig, SceneId,
};
use bop_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bop_physics_rapier::PreparedPhysicsAsset;
use bop_pipeline::{
    CancellationToken, ParallelGenerationPipeline, ParallelSnapshotRenderer,
    ParallelSnapshotSimulator, SerialSnapshotRenderer, SerialSnapshotSimulator,
};
use bop_pipeline::{PipelineRenderError, SerialDatasetRenderer, SerialGenerationPipeline};
use bop_render_api::Renderer;

const REQUEST_JSON: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":2,"image_size":{"width":2,"height":2},"k":[2.0,0.0,1.0,0.0,2.0,1.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.1,0.2,0.3,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":0.25},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;

struct FakeRenderer {
    prepared: bool,
    capabilities: RendererCapabilities,
    cancel_after_render: Option<CancellationToken>,
    label_instances: bool,
}

impl FakeRenderer {
    fn all_channels() -> Self {
        Self {
            prepared: false,
            cancel_after_render: None,
            label_instances: false,
            capabilities: RendererCapabilities::new(
                RenderChannels::try_new(true, true, true).unwrap(),
                None,
            ),
        }
    }

    /// Marks every even pixel with the scene's first instance ID.
    fn labelled() -> Self {
        let mut renderer = Self::all_channels();
        renderer.label_instances = true;
        renderer
    }

    fn rgb_only() -> Self {
        Self {
            prepared: false,
            cancel_after_render: None,
            label_instances: false,
            capabilities: RendererCapabilities::new(
                RenderChannels::try_new(true, false, false).unwrap(),
                None,
            ),
        }
    }

    fn cancelling(token: CancellationToken) -> Self {
        let mut renderer = Self::all_channels();
        renderer.cancel_after_render = Some(token);
        renderer
    }
}

impl Renderer for FakeRenderer {
    fn info(&self) -> BackendInfo {
        BackendInfo::try_new(
            BackendKind::Wgpu,
            "fake-v1",
            None,
            BTreeMap::from([("test_double".to_owned(), "true".to_owned())]),
        )
        .unwrap()
    }

    fn capabilities(&self) -> RendererCapabilities {
        self.capabilities
    }

    fn prepare(&mut self, _assets: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        self.prepared = true;
        Ok(())
    }

    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &SceneStagingArea,
    ) -> Result<RenderReport, RenderError> {
        if !self.prepared {
            return Err(RenderError::NotPrepared);
        }
        for camera in request.cameras() {
            let size = camera.image_size();
            let pixel_count = usize::try_from(u64::from(size.width()) * u64::from(size.height()))
                .map_err(|error| RenderError::Internal {
                message: error.to_string(),
            })?;
            staging
                .prepare_camera(camera.camera_id())
                .map_err(|error| RenderError::Io {
                    operation: "prepare fake camera".to_owned(),
                    message: error.to_string(),
                })?;
            RgbFrame::try_new(size.width(), size.height(), vec![17; pixel_count * 3])
                .unwrap()
                .write_png(
                    &staging.rgb_path(camera.camera_id()),
                    request.png_compression(),
                )
                .unwrap();
            DepthFrame::try_new(size.width(), size.height(), vec![1.25; pixel_count])
                .unwrap()
                .write_exr(&staging.depth_path(camera.camera_id()))
                .unwrap();
            let label = request
                .scene()
                .instances()
                .first()
                .filter(|_| self.label_instances)
                .map_or(0, |instance| instance.instance_id().get());
            let ids = (0..pixel_count)
                .map(|pixel| if pixel % 2 == 0 { label } else { 0 })
                .collect();
            InstanceFrame::try_new(size.width(), size.height(), ids)
                .unwrap()
                .write_png(
                    &staging.instance_path(camera.camera_id()),
                    request.png_compression(),
                )
                .unwrap();
        }
        if let Some(token) = &self.cancel_after_render {
            token.cancel();
        }
        let zero = NonNegativeFinite::try_new(0.0).unwrap();
        Ok(RenderReport::new(
            request.scene().scene_id(),
            self.info(),
            NonZeroU32::new(u32::try_from(request.cameras().len()).unwrap()).unwrap(),
            request.required_channels(),
            RenderStageTimings::new(zero, zero, zero, zero),
            0,
        ))
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
}

fn catalog() -> ResolvedAssetCatalog {
    let visual = VisualPrimitive::try_new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0.0, 0.0, 1.0]; 3],
        vec![],
        vec![0, 1, 2],
        None,
    )
    .unwrap();
    let collision = ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .unwrap();
    let asset = Arc::new(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![visual]).unwrap(),
            vec![],
            vec![],
            vec![collision],
        )
        .unwrap(),
    );
    ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
        AssetId::new("cube").unwrap(),
        *blake3::hash(b"tetrahedron").as_bytes(),
        asset,
    )])
    .unwrap()
}

fn prepared_assets() -> BTreeMap<AssetId, Arc<PreparedPhysicsAsset>> {
    let part = ConvexPart::try_new(
        vec![
            [-0.05, -0.05, -0.05],
            [0.05, -0.05, -0.05],
            [0.05, 0.05, -0.05],
            [-0.05, 0.05, -0.05],
            [-0.05, -0.05, 0.05],
            [0.05, -0.05, 0.05],
            [0.05, 0.05, 0.05],
            [-0.05, 0.05, 0.05],
        ],
        vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ],
    )
    .unwrap();
    BTreeMap::from([(
        AssetId::new("cube").unwrap(),
        Arc::new(PreparedPhysicsAsset::try_new(&[part]).unwrap()),
    )])
}

#[test]
fn fake_renderer_exercises_prepare_render_encode_and_atomic_commit() {
    let root = tempfile::tempdir().unwrap();
    let request: RenderSceneRequest = serde_json::from_str(REQUEST_JSON).unwrap();
    let mut pipeline = SerialDatasetRenderer::new(FakeRenderer::all_channels(), root.path());
    assert!(matches!(
        pipeline.render_and_commit(&request),
        Err(PipelineRenderError::Render(RenderError::NotPrepared))
    ));
    pipeline.prepare(&catalog()).unwrap();

    let completed = pipeline.render_and_commit(&request).unwrap();
    assert_eq!(completed.report().scene_id(), request.scene().scene_id());
    assert!(completed.scene_path().join("scene.json").is_file());
    assert!(
        completed
            .scene_path()
            .join("frames/00002/frame.json")
            .is_file()
    );
    let checksums: serde_json::Value = serde_json::from_slice(
        &std::fs::read(completed.scene_path().join("checksums.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(checksums["schema_version"], 1);
    assert_eq!(checksums["scene_id"], 9);
    assert_eq!(checksums["files"].as_object().unwrap().len(), 5);
    assert!(
        completed
            .scene_path()
            .join("frames/00002/rgb.png")
            .is_file()
    );
    assert!(
        completed
            .scene_path()
            .join("frames/00002/depth.exr")
            .is_file()
    );
    assert!(
        completed
            .scene_path()
            .join("frames/00002/instance.png")
            .is_file()
    );
    assert!(
        root.path()
            .join(".staging")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn capability_failure_happens_before_any_staging_side_effect() {
    let root = tempfile::tempdir().unwrap();
    let request: RenderSceneRequest = serde_json::from_str(REQUEST_JSON).unwrap();
    let mut pipeline = SerialDatasetRenderer::new(FakeRenderer::rgb_only(), root.path());

    assert!(matches!(
        pipeline.render_and_commit(&request),
        Err(PipelineRenderError::Render(
            RenderError::UnsupportedCapabilities { .. }
        ))
    ));
    assert!(!root.path().join(".staging").exists());
}

#[test]
fn serial_generation_composes_physics_render_validation_and_commit() {
    let root = tempfile::tempdir().unwrap();
    let plan = RunConfig::from_toml(include_str!("fixtures/serial_run.toml"))
        .unwrap()
        .into_plan();
    let mut pipeline = SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::all_channels(),
        root.path(),
    )
    .unwrap();
    pipeline.prepare(&catalog()).unwrap();

    let completed = pipeline.generate_and_commit(SceneId::new(0)).unwrap();
    let scene: serde_json::Value =
        serde_json::from_slice(&std::fs::read(completed.scene_path().join("scene.json")).unwrap())
            .unwrap();
    assert_eq!(scene["scene_id"], 0);
    assert_eq!(scene["instances"].as_array().unwrap().len(), 1);
    assert!(completed.scene_path().join("checksums.json").is_file());
}

#[test]
fn serial_run_writes_provenance_and_resumes_without_duplicate_generation() {
    let root = tempfile::tempdir().unwrap();
    let resolved_config = include_bytes!("fixtures/serial_run.toml");
    let plan = RunConfig::from_toml(std::str::from_utf8(resolved_config).unwrap())
        .unwrap()
        .into_plan();
    let visual_catalog = catalog();

    let mut first = SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::all_channels(),
        root.path(),
    )
    .unwrap();
    let first_report = first
        .run_to_completion(resolved_config, &visual_catalog)
        .unwrap();
    assert_eq!(first_report.preexisting_scene_count(), 0);
    assert_eq!(first_report.generated_scene_count(), 1);
    assert_eq!(first_report.total_scene_count(), 1);

    let run: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("run.json")).unwrap()).unwrap();
    assert_eq!(run["total_scene_count"], 1);
    assert_eq!(
        run["asset_catalog_fingerprint"],
        visual_catalog.fingerprint()
    );
    assert_eq!(run["backend"]["implementation_version"], "fake-v1");
    assert_eq!(
        std::fs::read(root.path().join("resolved-config.toml")).unwrap(),
        resolved_config
    );
    let environment: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("environment.json")).unwrap())
            .unwrap();
    assert_eq!(environment["schema_version"], 1);
    assert_eq!(environment["generator_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(environment["target_os"], std::env::consts::OS);
    assert_eq!(environment["target_arch"], std::env::consts::ARCH);

    let mut resumed = SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::all_channels(),
        root.path(),
    )
    .unwrap();
    let resumed_report = resumed
        .run_to_completion(resolved_config, &visual_catalog)
        .unwrap();
    assert_eq!(resumed_report.preexisting_scene_count(), 1);
    assert_eq!(resumed_report.generated_scene_count(), 0);
    assert_eq!(resumed_report.total_scene_count(), 1);
    assert_eq!(root.path().join("scenes").read_dir().unwrap().count(), 1);
}

#[test]
fn serial_run_stops_allocating_scenes_after_cancellation_and_can_resume() {
    let root = tempfile::tempdir().unwrap();
    let resolved_config = include_str!("fixtures/serial_run.toml")
        .replace("valid_scene_count = 1", "valid_scene_count = 2");
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let cancellation = CancellationToken::new();
    let mut interrupted = SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::cancelling(cancellation.clone()),
        root.path(),
    )
    .unwrap();

    let interrupted_report = interrupted
        .run_until_cancelled(resolved_config.as_bytes(), &visual_catalog, &cancellation)
        .unwrap();
    assert!(interrupted_report.was_cancelled());
    assert_eq!(interrupted_report.generated_scene_count(), 1);
    assert_eq!(root.path().join("scenes").read_dir().unwrap().count(), 1);

    let mut resumed = SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::all_channels(),
        root.path(),
    )
    .unwrap();
    let resumed_report = resumed
        .run_to_completion(resolved_config.as_bytes(), &visual_catalog)
        .unwrap();
    assert!(!resumed_report.was_cancelled());
    assert_eq!(resumed_report.preexisting_scene_count(), 1);
    assert_eq!(resumed_report.generated_scene_count(), 1);
    assert_eq!(root.path().join("scenes").read_dir().unwrap().count(), 2);
}

#[test]
fn simulation_corpus_feeds_render_only_pipeline_without_physics_reexecution() {
    let snapshots_root = tempfile::tempdir().unwrap();
    let dataset_root = tempfile::tempdir().unwrap();
    let resolved_config = include_str!("fixtures/serial_run.toml")
        .replace("valid_scene_count = 1", "valid_scene_count = 2");
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let cancellation = CancellationToken::new();
    let simulator = SerialSnapshotSimulator::try_new(&plan, prepared_assets()).unwrap();

    let visual_catalog = catalog();
    let simulation = simulator
        .run(
            resolved_config.as_bytes(),
            visual_catalog.fingerprint(),
            snapshots_root.path(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(simulation.generated_scene_count(), 2);

    let incompatible = simulator.run(
        b"different resolved config",
        visual_catalog.fingerprint(),
        snapshots_root.path(),
        &cancellation,
    );
    assert!(incompatible.is_err());
    assert_eq!(
        fs::read(snapshots_root.path().join("resolved-config.toml")).unwrap(),
        resolved_config.as_bytes()
    );
    assert!(snapshots_root.path().join("environment.json").is_file());

    let incompatible_dataset = tempfile::tempdir().unwrap();
    let mut incompatible_renderer = SerialSnapshotRenderer::new(
        &plan,
        FakeRenderer::all_channels(),
        incompatible_dataset.path(),
    );
    assert!(
        incompatible_renderer
            .run_to_completion(
                b"different resolved config",
                &visual_catalog,
                snapshots_root.path(),
            )
            .is_err()
    );
    assert!(!incompatible_dataset.path().join("run.json").exists());

    let mut renderer =
        SerialSnapshotRenderer::new(&plan, FakeRenderer::all_channels(), dataset_root.path());
    let rendering = renderer
        .run_to_completion(
            resolved_config.as_bytes(),
            &visual_catalog,
            snapshots_root.path(),
        )
        .unwrap();
    assert_eq!(rendering.generated_scene_count(), 2);
    assert_eq!(
        dataset_root
            .path()
            .join("scenes")
            .read_dir()
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn render_only_pipeline_stops_after_the_in_flight_scene_and_resumes() {
    let snapshots_root = tempfile::tempdir().unwrap();
    let dataset_root = tempfile::tempdir().unwrap();
    let resolved_config = include_str!("fixtures/serial_run.toml")
        .replace("valid_scene_count = 1", "valid_scene_count = 2");
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let simulator = SerialSnapshotSimulator::try_new(&plan, prepared_assets()).unwrap();
    simulator
        .run(
            resolved_config.as_bytes(),
            visual_catalog.fingerprint(),
            snapshots_root.path(),
            &CancellationToken::new(),
        )
        .unwrap();

    let cancellation = CancellationToken::new();
    let mut interrupted = SerialSnapshotRenderer::new(
        &plan,
        FakeRenderer::cancelling(cancellation.clone()),
        dataset_root.path(),
    );
    let report = interrupted
        .run_until_cancelled(
            resolved_config.as_bytes(),
            &visual_catalog,
            snapshots_root.path(),
            &cancellation,
        )
        .unwrap();
    assert!(report.was_cancelled());
    assert_eq!(report.generated_scene_count(), 1);

    let mut resumed =
        SerialSnapshotRenderer::new(&plan, FakeRenderer::all_channels(), dataset_root.path());
    let report = resumed
        .run_to_completion(
            resolved_config.as_bytes(),
            &visual_catalog,
            snapshots_root.path(),
        )
        .unwrap();
    assert_eq!(report.preexisting_scene_count(), 1);
    assert_eq!(report.generated_scene_count(), 1);
}

fn parallel_config(scene_count: u32) -> String {
    include_str!("fixtures/serial_run.toml")
        .replace(
            "valid_scene_count = 1",
            &format!("valid_scene_count = {scene_count}"),
        )
        .replace("simulation_workers = 1", "simulation_workers = 3")
        .replace("queue_capacity = 1", "queue_capacity = 2")
}

/// Reads every file under `root`, keyed by its path relative to `root`.
fn tree_bytes(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_owned();
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn parallel_simulation_writes_the_same_corpus_as_serial_simulation() {
    let resolved_config = parallel_config(5);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let fingerprint = catalog().fingerprint().to_owned();
    let serial_root = tempfile::tempdir().unwrap();
    let parallel_root = tempfile::tempdir().unwrap();

    let serial = SerialSnapshotSimulator::try_new(&plan, prepared_assets())
        .unwrap()
        .run(
            resolved_config.as_bytes(),
            &fingerprint,
            serial_root.path(),
            &CancellationToken::new(),
        )
        .unwrap();
    let parallel = ParallelSnapshotSimulator::try_new(&plan, prepared_assets())
        .unwrap()
        .run(
            resolved_config.as_bytes(),
            &fingerprint,
            parallel_root.path(),
            &CancellationToken::new(),
        )
        .unwrap();

    assert_eq!(serial, parallel);
    assert_eq!(parallel.generated_scene_count(), 5);
    let scenes = tree_bytes(&parallel_root.path().join("scenes"));
    assert_eq!(scenes.len(), 5);
    assert_eq!(tree_bytes(&serial_root.path().join("scenes")), scenes);
}

#[test]
fn parallel_simulation_resumes_without_regenerating_existing_snapshots() {
    let resolved_config = parallel_config(4);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let fingerprint = catalog().fingerprint().to_owned();
    let root = tempfile::tempdir().unwrap();
    let simulator = ParallelSnapshotSimulator::try_new(&plan, prepared_assets()).unwrap();

    let first = simulator
        .run(
            resolved_config.as_bytes(),
            &fingerprint,
            root.path(),
            &CancellationToken::new(),
        )
        .unwrap();
    let second = simulator
        .run(
            resolved_config.as_bytes(),
            &fingerprint,
            root.path(),
            &CancellationToken::new(),
        )
        .unwrap();

    assert_eq!(first.generated_scene_count(), 4);
    assert_eq!(second.preexisting_scene_count(), 4);
    assert_eq!(second.generated_scene_count(), 0);
}

#[test]
fn parallel_generation_commits_the_same_scenes_as_serial_generation() {
    let resolved_config = parallel_config(5);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let serial_root = tempfile::tempdir().unwrap();
    let parallel_root = tempfile::tempdir().unwrap();

    SerialGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        FakeRenderer::all_channels(),
        serial_root.path(),
    )
    .unwrap()
    .run_to_completion(resolved_config.as_bytes(), &visual_catalog)
    .unwrap();
    let report = ParallelGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        [FakeRenderer::all_channels(), FakeRenderer::all_channels()],
        parallel_root.path(),
    )
    .unwrap()
    .run_to_completion(resolved_config.as_bytes(), &visual_catalog)
    .unwrap();

    assert_eq!(report.generated_scene_count(), 5);
    assert!(!report.was_cancelled());
    let scenes = |root: &std::path::Path| {
        (0..5)
            .map(|id| fs::read(root.join(format!("scenes/{id:020}/scene.json"))).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(scenes(parallel_root.path()), scenes(serial_root.path()));
}

#[test]
fn parallel_generation_stops_after_cancellation_and_resumes() {
    let resolved_config = parallel_config(6);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let root = tempfile::tempdir().unwrap();
    let cancellation = CancellationToken::new();

    let interrupted = ParallelGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        [FakeRenderer::cancelling(cancellation.clone())],
        root.path(),
    )
    .unwrap()
    .run_until_cancelled(resolved_config.as_bytes(), &visual_catalog, &cancellation)
    .unwrap();
    assert!(interrupted.was_cancelled());
    assert_eq!(interrupted.generated_scene_count(), 1);
    assert!(
        root.path()
            .join(".staging")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );

    let resumed = ParallelGenerationPipeline::try_new(
        &plan,
        prepared_assets(),
        [FakeRenderer::all_channels(), FakeRenderer::all_channels()],
        root.path(),
    )
    .unwrap()
    .run_to_completion(resolved_config.as_bytes(), &visual_catalog)
    .unwrap();
    assert_eq!(resumed.preexisting_scene_count(), 1);
    assert_eq!(resumed.generated_scene_count(), 5);
    assert_eq!(root.path().join("scenes").read_dir().unwrap().count(), 6);
}

#[test]
fn parallel_render_only_pipeline_matches_serial_rendering() {
    let resolved_config = parallel_config(5);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let corpus = tempfile::tempdir().unwrap();
    let serial_root = tempfile::tempdir().unwrap();
    let parallel_root = tempfile::tempdir().unwrap();
    ParallelSnapshotSimulator::try_new(&plan, prepared_assets())
        .unwrap()
        .run(
            resolved_config.as_bytes(),
            visual_catalog.fingerprint(),
            corpus.path(),
            &CancellationToken::new(),
        )
        .unwrap();

    SerialSnapshotRenderer::new(&plan, FakeRenderer::all_channels(), serial_root.path())
        .run_to_completion(resolved_config.as_bytes(), &visual_catalog, corpus.path())
        .unwrap();
    let report = ParallelSnapshotRenderer::try_new(
        &plan,
        (0..3).map(|_| FakeRenderer::all_channels()),
        parallel_root.path(),
    )
    .unwrap()
    .run_to_completion(resolved_config.as_bytes(), &visual_catalog, corpus.path())
    .unwrap();

    assert_eq!(report.generated_scene_count(), 5);
    assert_eq!(
        tree_bytes(&parallel_root.path().join("scenes")),
        tree_bytes(&serial_root.path().join("scenes"))
    );
}

#[test]
fn a_parallel_pipeline_needs_at_least_one_renderer() {
    let plan = RunConfig::from_toml(&parallel_config(1))
        .unwrap()
        .into_plan();
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        ParallelSnapshotRenderer::try_new(&plan, Vec::<FakeRenderer>::new(), root.path()),
        Err(bop_pipeline::SnapshotRenderError::RendererPool(
            bop_pipeline::RendererPoolError::Empty
        ))
    ));
}

fn decode_png(path: &std::path::Path) -> Vec<u8> {
    let mut reader = png::Decoder::new(std::io::BufReader::new(fs::File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    bytes.truncate(info.buffer_size());
    bytes
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn quaternion(value: &serde_json::Value) -> glam::DQuat {
    let q = value["rotation_xyzw"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect::<Vec<_>>();
    glam::DQuat::from_xyzw(q[0], q[1], q[2], q[3])
}

fn vector(value: &serde_json::Value) -> glam::DVec3 {
    let v = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect::<Vec<_>>();
    glam::DVec3::new(v[0], v[1], v[2])
}

#[test]
fn bop_export_round_trips_models_poses_depth_and_masks() {
    use bop_dataset::{BopExportError, BopExportOptions, BopReport, export_bop, validate_bop};

    let resolved_config = parallel_config(3);
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let root = tempfile::tempdir().unwrap();
    let dataset = root.path().join("dataset");
    SerialGenerationPipeline::try_new(&plan, prepared_assets(), FakeRenderer::labelled(), &dataset)
        .unwrap()
        .run_to_completion(resolved_config.as_bytes(), &visual_catalog)
        .unwrap();

    let models = bop_dataset::read_canonical_models(&dataset).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].positions(),
        visual_catalog
            .iter()
            .next()
            .unwrap()
            .asset()
            .visual_mesh()
            .primitives()[0]
            .positions()
    );

    let out = root.path().join("bop");
    let report = export_bop(&dataset, &out, BopExportOptions::default()).unwrap();
    let expected = BopReport {
        object_count: 1,
        scene_count: 3,
        image_count: 3,
        ground_truth_count: 3,
    };
    assert_eq!(report, expected);
    assert_eq!(validate_bop(&out).unwrap(), expected);

    // The model is the canonical mesh in millimetres.
    let info = read_json(&out.join("models/models_info.json"));
    assert!((info["1"]["diameter"].as_f64().unwrap() - 1000.0 * 2.0_f64.sqrt()).abs() < 1e-3);

    for scene_id in 0..3_u64 {
        assert_bop_scene_matches(&dataset, &out, scene_id);
    }

    assert!(matches!(
        export_bop(&dataset, &out, BopExportOptions::default()),
        Err(BopExportError::OutputExists { .. })
    ));
    let overflow = root.path().join("overflow");
    assert!(matches!(
        export_bop(
            &dataset,
            &overflow,
            BopExportOptions::try_new(0.01).unwrap()
        ),
        Err(BopExportError::DepthOverflow { .. })
    ));
    let names = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names, ["bop".to_owned(), "dataset".to_owned()].into());

    fs::write(
        dataset.join(format!("scenes/{:020}/frames/00001/rgb.png", 0)),
        b"x",
    )
    .unwrap();
    assert!(matches!(
        export_bop(
            &dataset,
            &root.path().join("tampered"),
            BopExportOptions::default()
        ),
        Err(BopExportError::Dataset(_))
    ));
}

/// Checks one exported BOP scene against its canonical scene, computed independently.
fn assert_bop_scene_matches(dataset: &std::path::Path, out: &std::path::Path, scene_id: u64) {
    let canonical = dataset.join(format!("scenes/{scene_id:020}"));
    let scene = read_json(&canonical.join("scene.json"));
    let frame = read_json(&canonical.join("frames/00001/frame.json"));
    let bop = out.join(format!("train_pbr/{scene_id:06}"));

    // Recompute camera_from_model independently in f64.
    let camera = &frame["camera_from_world"]["camera_from_world"];
    let model = &scene["instances"][0]["world_from_model"];
    let (q_c, t_c) = (quaternion(camera), vector(&camera["translation"]));
    let (q_m, t_m) = (quaternion(model), vector(&model["translation"]));
    let rotation = glam::DMat3::from_quat(q_c * q_m);
    let translation = (q_c * t_m + t_c) * 1000.0;
    let gt = &read_json(&bop.join("scene_gt.json"))["1"][0];
    assert_eq!(gt["obj_id"], 1);
    let r = gt["cam_R_m2c"].as_array().unwrap();
    for row in 0..3 {
        for column in 0..3 {
            let exported = r[row * 3 + column].as_f64().unwrap();
            let difference = (exported - rotation.col(column)[row]).abs();
            // Canonical poses are stored as f32 quaternions.
            assert!(difference < 1e-6, "rotation differs by {difference:e}");
        }
    }
    assert!((vector(&gt["cam_t_m2c"]) - translation).length() < 1e-3);

    let cameras = read_json(&bop.join("scene_camera.json"));
    assert_eq!(cameras["1"]["depth_scale"], 0.1);
    assert_eq!(cameras["1"]["cam_K"], frame["k"]);

    // 1.25 m at 0.1 mm per unit.
    let depth = decode_png(&bop.join("depth/000001.png"));
    assert!(
        depth
            .chunks_exact(2)
            .all(|unit| u16::from_be_bytes([unit[0], unit[1]]) == 12_500)
    );
    let mask = decode_png(&bop.join("mask_visib/000001_000000.png"));
    assert_eq!(mask.len(), 16 * 12);
    assert!(
        mask.iter()
            .enumerate()
            .all(|(pixel, value)| *value == if pixel % 2 == 0 { 255 } else { 0 })
    );
    assert_eq!(
        fs::read(bop.join("rgb/000001.png")).unwrap(),
        fs::read(canonical.join("frames/00001/rgb.png")).unwrap()
    );
}

#[test]
fn benchmark_backend_measures_warmed_repetitions_and_reports_them() {
    use bop_pipeline::{
        BackendBenchmark, BenchmarkBackend, BenchmarkHost, BenchmarkReport, PhysicsBenchmark,
        PreparationBenchmark, RendererBenchmark,
    };

    let resolved_config = include_str!("fixtures/serial_run.toml")
        .replace("valid_scene_count = 1", "valid_scene_count = 3");
    let plan = RunConfig::from_toml(&resolved_config).unwrap().into_plan();
    let visual_catalog = catalog();
    let corpus = tempfile::tempdir().unwrap();
    SerialSnapshotSimulator::try_new(&plan, prepared_assets())
        .unwrap()
        .run(
            resolved_config.as_bytes(),
            visual_catalog.fingerprint(),
            corpus.path(),
            &CancellationToken::new(),
        )
        .unwrap();
    let scenes = bop_dataset::read_snapshot_corpus(corpus.path()).unwrap();
    let work = tempfile::tempdir().unwrap();
    let stale = work.path().join("runs/repetition-9");
    fs::create_dir_all(&stale).unwrap();

    let mut backend = RendererBenchmark::try_new(
        FakeRenderer::all_channels(),
        &plan,
        resolved_config.as_bytes(),
        &visual_catalog,
        &scenes,
        work.path().join("runs"),
        5,
    )
    .unwrap();
    assert_eq!(backend.warmup_scenes(), 3);
    assert!(!stale.exists());
    let samples = (0..2)
        .map(|repetition| backend.measure(repetition, 0).unwrap())
        .collect::<Vec<_>>();
    for sample in &samples {
        assert_eq!(sample.scene_seconds.len(), 3);
        assert!(sample.scenes_per_second > 0.0 && sample.output_bytes > 0);
    }
    // Only the latest repetition's dataset is kept, and it is complete.
    let runs = work.path().join("runs");
    assert!(!runs.join("warmup").exists() && !runs.join("repetition-0").exists());
    assert_eq!(
        bop_dataset::validate_dataset(&runs.join("repetition-1"))
            .unwrap()
            .scene_count(),
        3
    );

    let summary = BackendBenchmark::try_new(&backend, samples).unwrap();
    assert_eq!(summary.repetitions.len(), 2);
    assert!(summary.scene_latency_p95_seconds >= summary.scene_latency_p50_seconds);
    let report = BenchmarkReport::new(
        "fixture".to_owned(),
        bop_dataset::fingerprint_config(resolved_config.as_bytes()),
        1,
        vec![vec![BackendKind::Wgpu]; 2],
        BenchmarkHost::default(),
        PreparationBenchmark {
            first_seconds: 0.5,
            repeat_seconds: 0.1,
            cache_preexisting: false,
        },
        PhysicsBenchmark {
            scene_count: 3,
            workers: 1,
            reused_corpus: false,
            wall_seconds: Some(1.0),
            scenes_per_second: Some(3.0),
            steps_per_scene: 10.0,
            rejection_rate: 0.0,
        },
        vec![summary],
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["repetitions"], 2);
    assert_eq!(json["backends"][0]["backend"]["kind"], "wgpu");
    assert!(
        json["backends"][0]["repetitions"][0]
            .get("scene_seconds")
            .is_none()
    );
    let markdown = report.to_markdown();
    assert!(markdown.starts_with("# Benchmark: fixture"));
    assert!(markdown.contains("| Wgpu | - |"));
    assert!(markdown.contains("cache was cold"));
}
