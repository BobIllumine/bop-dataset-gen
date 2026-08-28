//! Persistent worker lifecycle and canonical output acceptance tests.

#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt as _, sync::Arc};

    use bpp_assets::{
        ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh,
        VisualPrimitive,
    };
    use bpp_core::{AssetId, BlenderProcRendererConfig, RenderSceneRequest, Renderer};
    use bpp_dataset::SceneStagingArea;
    use bpp_render_blenderproc::BlenderProcRenderer;

    const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":7,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"triangle","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":2,"height":2},"k":[2.0,0.0,1.0,0.0,2.0,1.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;

    fn catalog() -> ResolvedAssetCatalog {
        let primitive = VisualPrimitive::try_new(
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
                VisualMesh::try_new(vec![primitive]).unwrap(),
                vec![],
                vec![],
                vec![collision],
            )
            .unwrap(),
        );
        ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
            AssetId::new("triangle").unwrap(),
            *blake3::hash(b"triangle").as_bytes(),
            asset,
        )])
        .unwrap()
    }

    #[test]
    fn persistent_worker_prepares_renders_encodes_and_flushes() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("fake-worker.py");
        fs::write(&executable, r#"#!/usr/bin/env python3
import json, pathlib, struct, sys
for line in sys.stdin:
 m=json.loads(line); t=m['type']; v={'protocol_version':1}
 if t=='hello': r={**v,'type':'ready','implementation_version':'fake-1','device_name':'CPU'}
 elif t=='prepare': r={**v,'type':'prepared','request_id':m['request_id'],'catalog_fingerprint':m['catalog_fingerprint']}
 elif t=='render':
  c=m['request']['cameras'][0]; d=pathlib.Path(m['output_root'])/'frames'/f"{c['camera_id']:05d}"; d.mkdir(parents=True,exist_ok=True)
  (d/'rgb.u8').write_bytes(bytes([255,0,0]*4)); (d/'depth.f32le').write_bytes(struct.pack('<4f',2,2,2,2)); (d/'instance.u16le').write_bytes(struct.pack('<4H',1,1,1,1))
  r={**v,'type':'rendered','request_id':m['request_id'],'scene_id':m['request']['scene']['scene_id'],'render_seconds':0.01}
 elif t=='flush': r={**v,'type':'flushed','request_id':m['request_id']}
 elif t=='shutdown': break
 print(json.dumps(r),flush=True)
"#).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let config =
            BlenderProcRendererConfig::try_new(&executable, "unused.py", "CPU", 2.0, 2.0, 4096)
                .unwrap();
        let mut renderer = BlenderProcRenderer::try_new(config).unwrap();
        assert_eq!(renderer.info().implementation_version(), "fake-1");
        renderer.prepare(&catalog()).unwrap();
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let staging = SceneStagingArea::create(temp.path(), request.scene().scene_id()).unwrap();
        staging
            .prepare_camera(request.cameras()[0].camera_id())
            .unwrap();
        let report = renderer.render_scene(&request, &staging).unwrap();
        assert_eq!(report.scene_id(), request.scene().scene_id());
        assert!(staging.rgb_path(request.cameras()[0].camera_id()).is_file());
        assert!(
            staging
                .depth_path(request.cameras()[0].camera_id())
                .is_file()
        );
        assert!(
            staging
                .instance_path(request.cameras()[0].camera_id())
                .is_file()
        );
        assert!(
            !staging
                .camera_dir(request.cameras()[0].camera_id())
                .join("rgb.u8")
                .exists()
        );
        renderer.flush().unwrap();
    }

    #[test]
    fn unexpected_render_crash_is_restarted_exactly_once_and_reprepared() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("crash-once-worker.py");
        fs::write(&executable, r#"#!/usr/bin/env python3
import json, pathlib, struct, sys
marker=pathlib.Path(__file__).with_suffix('.crashed')
for line in sys.stdin:
 m=json.loads(line); t=m['type']; v={'protocol_version':1}
 if t=='hello': r={**v,'type':'ready','implementation_version':'fake-1','device_name':'CPU'}
 elif t=='prepare': r={**v,'type':'prepared','request_id':m['request_id'],'catalog_fingerprint':m['catalog_fingerprint']}
 elif t=='render':
  if not marker.exists(): marker.write_text('once'); sys.exit(9)
  c=m['request']['cameras'][0]; d=pathlib.Path(m['output_root'])/'frames'/f"{c['camera_id']:05d}"; d.mkdir(parents=True,exist_ok=True)
  (d/'rgb.u8').write_bytes(bytes([255,0,0]*4)); (d/'depth.f32le').write_bytes(struct.pack('<4f',2,2,2,2)); (d/'instance.u16le').write_bytes(struct.pack('<4H',1,1,1,1))
  r={**v,'type':'rendered','request_id':m['request_id'],'scene_id':m['request']['scene']['scene_id'],'render_seconds':0.01}
 elif t=='shutdown': break
 else: r={**v,'type':'flushed','request_id':m['request_id']}
 print(json.dumps(r),flush=True)
"#).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let config =
            BlenderProcRendererConfig::try_new(&executable, "unused.py", "CPU", 2.0, 2.0, 4096)
                .unwrap();
        let mut renderer = BlenderProcRenderer::try_new(config).unwrap();
        renderer.prepare(&catalog()).unwrap();
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let staging = SceneStagingArea::create(temp.path(), request.scene().scene_id()).unwrap();
        staging
            .prepare_camera(request.cameras()[0].camera_id())
            .unwrap();
        assert!(renderer.render_scene(&request, &staging).is_ok());
        assert!(executable.with_extension("crashed").is_file());
    }
}
