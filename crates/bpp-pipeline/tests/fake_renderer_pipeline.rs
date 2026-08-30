//! Complete renderer-boundary to atomic-dataset acceptance test.

use std::{collections::BTreeMap, num::NonZeroU32, sync::Arc};

use bpp_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh, VisualPrimitive,
};
use bpp_core::{
    AssetId, BackendInfo, BackendKind, NonNegativeFinite, RenderChannels, RenderError,
    RenderReport, RenderSceneRequest, RenderStageTimings, Renderer, RendererCapabilities,
    RunConfig, SceneId,
};
use bpp_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bpp_physics_rapier::PreparedPhysicsAsset;
use bpp_pipeline::CancellationToken;
use bpp_pipeline::{PipelineRenderError, SerialDatasetRenderer, SerialGenerationPipeline};

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
    type AssetCatalog = ResolvedAssetCatalog;
    type StagingArea = SceneStagingArea;

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

    fn prepare(&mut self, _assets: &Self::AssetCatalog) -> Result<(), RenderError> {
        self.prepared = true;
        Ok(())
    }

    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &Self::StagingArea,
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
