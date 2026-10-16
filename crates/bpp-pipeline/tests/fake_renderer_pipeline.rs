//! Complete renderer-boundary to atomic-dataset acceptance test.

use std::{collections::BTreeMap, fs, num::NonZeroU32, sync::Arc};

use bpp_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh, VisualPrimitive,
};
use bpp_core::{
    AssetId, BackendInfo, BackendKind, NonNegativeFinite, RenderChannels, RenderError,
    RenderReport, RenderSceneRequest, RenderStageTimings, RendererCapabilities, RunConfig, SceneId,
};
use bpp_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bpp_physics_rapier::PreparedPhysicsAsset;
use bpp_pipeline::{
    CancellationToken, ParallelGenerationPipeline, ParallelSnapshotRenderer,
    ParallelSnapshotSimulator, SerialSnapshotRenderer, SerialSnapshotSimulator,
};
use bpp_pipeline::{PipelineRenderError, SerialDatasetRenderer, SerialGenerationPipeline};
use bpp_render_api::Renderer;

const REQUEST_JSON: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":9,"accepted_attempt":2,"seed_fingerprint":"seed-abc","bin":{"schema_version":1,"inner_dimensions_m":[1.0,2.0,3.0],"floor_thickness_m":0.1,"wall_thickness_m":0.2,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":50,"simulated_seconds":1.0,"final_max_linear_speed_mps":0.1,"final_max_angular_speed_rad_s":0.2,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":2,"image_size":{"width":2,"height":2},"k":[2.0,0.0,1.0,0.0,2.0,1.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.1,0.2,0.3,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":0.25},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;

struct FakeRenderer {
    prepared: bool,
    capabilities: RendererCapabilities,
    cancel_after_render: Option<CancellationToken>,
}

impl FakeRenderer {
    fn all_channels() -> Self {
        Self {
            prepared: false,
            cancel_after_render: None,
            capabilities: RendererCapabilities::new(
                RenderChannels::try_new(true, true, true).unwrap(),
                None,
            ),
        }
    }

    fn rgb_only() -> Self {
        Self {
            prepared: false,
            cancel_after_render: None,
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
                .write_png(&staging.rgb_path(camera.camera_id()))
                .unwrap();
            DepthFrame::try_new(size.width(), size.height(), vec![1.25; pixel_count])
                .unwrap()
                .write_exr(&staging.depth_path(camera.camera_id()))
                .unwrap();
            InstanceFrame::try_new(size.width(), size.height(), vec![0; pixel_count])
                .unwrap()
                .write_png(&staging.instance_path(camera.camera_id()))
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
        Err(bpp_pipeline::SnapshotRenderError::RendererPool(
            bpp_pipeline::RendererPoolError::Empty
        ))
    ));
}
