//! Deterministic run-manifest and resume-index contracts.

use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Barrier},
    thread,
};

use bpp_core::{BackendInfo, BackendKind, SceneId};
use bpp_dataset::{
    DatasetManifest, DatasetOpenError, DatasetResumeIndex, RunEnvironment, persist_run_environment,
};

fn backend(version: &str) -> BackendInfo {
    BackendInfo::try_new(
        BackendKind::Wgpu,
        version,
        Some("test adapter".to_owned()),
        BTreeMap::from([("mode".to_owned(), "deterministic".to_owned())]),
    )
    .unwrap()
}

fn manifest(config: &[u8]) -> DatasetManifest {
    DatasetManifest::try_new(config, "catalog-abc", backend("0.1.0"), 4).unwrap()
}

#[test]
fn manifest_fingerprint_is_versioned_repeatable_and_content_sensitive() {
    let first = manifest(b"canonical resolved config");
    let second = manifest(b"canonical resolved config");
    let changed = manifest(b"canonical resolved config!");

    assert_eq!(first, second);
    assert_ne!(
        first.resolved_config_fingerprint(),
        changed.resolved_config_fingerprint()
    );
    assert_eq!(
        first.resolved_config_fingerprint(),
        "e29d1a7d5c42a553a8a6e4b4e490c3f3105e27e36afad358797e8d3427fa352c"
    );
}

#[test]
fn manifest_deserialization_preserves_all_checked_invariants() {
    let valid = serde_json::to_value(manifest(b"config")).unwrap();
    for (field, invalid) in [
        ("schema_version", serde_json::json!(2)),
        ("resolved_config_fingerprint", serde_json::json!("xyz")),
        ("asset_catalog_fingerprint", serde_json::json!("  ")),
        ("total_scene_count", serde_json::json!(0)),
    ] {
        let mut candidate = valid.clone();
        candidate[field] = invalid;
        assert!(serde_json::from_value::<DatasetManifest>(candidate).is_err());
    }
}

#[test]
fn first_open_writes_canonical_manifest_and_identical_resume_accepts_it() {
    let root = tempfile::tempdir().unwrap();
    let expected = manifest(b"config-v1");
    let first = DatasetResumeIndex::open_or_create(root.path(), expected.clone()).unwrap();
    assert!(first.committed_scenes().is_empty());
    assert_eq!(
        first.pending_scenes().collect::<Vec<_>>(),
        vec![
            SceneId::new(0),
            SceneId::new(1),
            SceneId::new(2),
            SceneId::new(3)
        ]
    );

    let bytes = fs::read(root.path().join("run.json")).unwrap();
    assert!(bytes.ends_with(b"\n"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["schema_version"],
        1
    );
    let resumed = DatasetResumeIndex::open_or_create(root.path(), expected).unwrap();
    assert_eq!(resumed.manifest(), first.manifest());
}

#[test]
fn incompatible_resume_is_rejected_before_scene_scanning() {
    let root = tempfile::tempdir().unwrap();
    DatasetResumeIndex::open_or_create(root.path(), manifest(b"one")).unwrap();

    assert!(matches!(
        DatasetResumeIndex::open_or_create(root.path(), manifest(b"two")),
        Err(DatasetOpenError::IncompatibleManifest { .. })
    ));
}

#[test]
fn resolved_config_is_atomically_persisted_and_must_match_manifest_bytes() {
    let root = tempfile::tempdir().unwrap();
    let index = DatasetResumeIndex::open_or_create(root.path(), manifest(b"config-v1")).unwrap();

    index
        .persist_resolved_config(root.path(), b"config-v1")
        .unwrap();
    index
        .persist_resolved_config(root.path(), b"config-v1")
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("resolved-config.toml")).unwrap(),
        b"config-v1"
    );
    assert!(matches!(
        index.persist_resolved_config(root.path(), b"config-v2"),
        Err(DatasetOpenError::IncompatibleResolvedConfig)
    ));
}

#[test]
fn environment_provenance_is_canonical_and_resume_compatible() {
    let root = tempfile::tempdir().unwrap();
    let environment = RunEnvironment::try_new(
        "0.1.0",
        "macos",
        "aarch64",
        "release",
        vec!["fast".to_owned(), "wgpu".to_owned()],
    )
    .unwrap();
    persist_run_environment(root.path(), &environment).unwrap();
    persist_run_environment(root.path(), &environment).unwrap();

    let bytes = fs::read(root.path().join("environment.json")).unwrap();
    assert!(bytes.ends_with(b"\n"));
    let stored: RunEnvironment = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stored, environment);

    let changed = RunEnvironment::try_new(
        "0.1.1",
        "macos",
        "aarch64",
        "release",
        vec!["fast".to_owned(), "wgpu".to_owned()],
    )
    .unwrap();
    assert!(matches!(
        persist_run_environment(root.path(), &changed),
        Err(DatasetOpenError::IncompatibleEnvironment)
    ));
}

#[test]
fn index_reports_only_canonical_committed_scene_directories() {
    let root = tempfile::tempdir().unwrap();
    DatasetResumeIndex::open_or_create(root.path(), manifest(b"config")).unwrap();
    fs::create_dir_all(root.path().join("scenes/00000000000000000003")).unwrap();
    fs::create_dir_all(root.path().join("scenes/00000000000000000001")).unwrap();
    fs::create_dir_all(root.path().join(".staging/abandoned")).unwrap();
    fs::write(
        root.path().join("scenes/notes.txt"),
        b"ignored regular file",
    )
    .unwrap();

    let index = DatasetResumeIndex::open_or_create(root.path(), manifest(b"config")).unwrap();
    assert_eq!(
        index.committed_scenes(),
        &[SceneId::new(1), SceneId::new(3)]
    );
    assert_eq!(
        index.pending_scenes().collect::<Vec<_>>(),
        vec![SceneId::new(0), SceneId::new(2)]
    );
}

#[test]
fn malformed_or_out_of_range_scene_directory_is_not_silently_resumed() {
    for name in ["3", "00000000000000000004", "0000000000000000000x"] {
        let root = tempfile::tempdir().unwrap();
        DatasetResumeIndex::open_or_create(root.path(), manifest(b"config")).unwrap();
        fs::create_dir_all(root.path().join("scenes").join(name)).unwrap();
        assert!(matches!(
            DatasetResumeIndex::open_or_create(root.path(), manifest(b"config")),
            Err(DatasetOpenError::InvalidSceneDirectory { .. })
        ));
    }
}

#[test]
fn concurrent_identical_open_never_observes_a_partial_manifest() {
    let root = tempfile::tempdir().unwrap();
    let root = Arc::new(root.path().to_owned());
    let barrier = Arc::new(Barrier::new(8));
    let large_backend = BackendInfo::try_new(
        BackendKind::Wgpu,
        "0.1.0",
        None,
        BTreeMap::from([("payload".to_owned(), "x".repeat(2_000_000))]),
    )
    .unwrap();
    let manifest =
        Arc::new(DatasetManifest::try_new(b"config", "catalog", large_backend, 1).unwrap());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            let manifest = Arc::clone(&manifest);
            thread::spawn(move || {
                barrier.wait();
                DatasetResumeIndex::open_or_create(&root, (*manifest).clone())
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap().unwrap();
    }
}
