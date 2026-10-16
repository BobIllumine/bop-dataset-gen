//! Process-level checks of the `bpp` command contract and exit codes.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const TETRAHEDRON_STL: &str = "solid tetrahedron\n\
    facet normal 0 0 -1\nouter loop\nvertex -1 -1 0\nvertex 0 1 0\nvertex 1 -1 0\nendloop\nendfacet\n\
    facet normal 0 -1 0\nouter loop\nvertex -1 -1 0\nvertex 1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    facet normal 1 1 1\nouter loop\nvertex 1 -1 0\nvertex 0 1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    facet normal -1 1 1\nouter loop\nvertex 0 1 0\nvertex -1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    endsolid tetrahedron\n";

fn bpp(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bpp"))
        .args(arguments)
        .output()
        .expect("spawn bpp binary")
}

fn exit_code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("bpp exited without a status code")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Writes a one-scene run configuration whose single asset is a small STL.
fn write_run(root: &Path) -> PathBuf {
    fs::write(root.join("cube.stl"), TETRAHEDRON_STL).expect("write STL asset");
    let cache = root.join("cache");
    let config = include_str!("../../bpp-pipeline/tests/fixtures/serial_run.toml")
        .replace("source_path = \"cube.glb\"", "source_path = \"cube.stl\"")
        .replace("source_unit_scale_m = 1.0", "source_unit_scale_m = 0.05")
        .replace(
            "cache_path = \"cache\"",
            &format!("cache_path = \"{}\"", cache.display()),
        );
    let path = root.join("run.toml");
    fs::write(&path, config).expect("write run configuration");
    path
}

#[test]
fn missing_subcommand_prints_usage_and_exits_2() {
    let output = bpp(&[]);
    assert_eq!(exit_code(&output), 2);
    assert!(stderr(&output).contains("usage: bpp"));
}

#[test]
fn unknown_subcommand_exits_2() {
    assert_eq!(exit_code(&bpp(&["frobnicate"])), 2);
}

#[test]
fn missing_required_flag_exits_2() {
    assert_eq!(exit_code(&bpp(&["simulate", "--config", "run.toml"])), 2);
}

#[test]
fn schema_prints_the_committed_json_schema() {
    let output = bpp(&["schema"]);
    assert_eq!(exit_code(&output), 0);
    let printed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("schema output is JSON");
    let committed: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/run-config.schema.json"))
            .expect("committed schema is JSON");
    assert_eq!(printed, committed);
}

#[test]
fn unimplemented_subcommands_exit_3() {
    let export = bpp(&["export", "bop", "dataset", "--output", "bop-out"]);
    assert_eq!(exit_code(&export), 3);
    assert!(stderr(&export).contains("export bop"));

    let bench = bpp(&[
        "bench",
        "--config",
        "run.toml",
        "--renderers",
        "wgpu",
        "--output",
        "reports",
    ]);
    assert_eq!(exit_code(&bench), 3);
    assert!(stderr(&bench).contains("bench"));
}

#[cfg(not(feature = "blenderproc"))]
#[test]
fn doctor_for_a_renderer_missing_from_the_build_exits_3() {
    assert_eq!(exit_code(&bpp(&["doctor", "--renderer", "blenderproc"])), 3);
}

#[test]
fn missing_configuration_file_exits_2() {
    let root = tempfile::tempdir().expect("temporary root");
    let missing = root.path().join("absent.toml");
    let output = bpp(&[
        "prepare-assets",
        "--config",
        missing.to_str().expect("UTF-8 path"),
    ]);
    assert_eq!(exit_code(&output), 2);
}

#[test]
fn validating_a_missing_dataset_exits_5() {
    let root = tempfile::tempdir().expect("temporary root");
    let missing = root.path().join("dataset");
    assert_eq!(
        exit_code(&bpp(&["validate", missing.to_str().expect("UTF-8 path")])),
        5
    );
}

#[test]
fn prepare_assets_reports_the_catalog() {
    let root = tempfile::tempdir().expect("temporary root");
    let config = write_run(root.path());
    let output = bpp(&[
        "prepare-assets",
        "--config",
        config.to_str().expect("UTF-8 path"),
    ]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report is JSON");
    assert_eq!(report["asset_count"], 1);
}

#[test]
fn simulate_writes_a_snapshot_corpus_without_a_renderer() {
    let root = tempfile::tempdir().expect("temporary root");
    let config = write_run(root.path());
    let corpus = root.path().join("corpus");
    let output = bpp(&[
        "simulate",
        "--config",
        config.to_str().expect("UTF-8 path"),
        "--output",
        corpus.to_str().expect("UTF-8 path"),
    ]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report is JSON");
    assert_eq!(report["generated_scenes"], 1);
    assert_eq!(report["cancelled"], false);
    assert!(corpus.is_dir());
}

#[test]
fn generate_refuses_an_existing_output_without_resume() {
    let root = tempfile::tempdir().expect("temporary root");
    let config = write_run(root.path());
    let dataset = root.path().join("dataset");
    fs::create_dir(&dataset).expect("pre-create output");
    let output = bpp(&[
        "generate",
        "--config",
        config.to_str().expect("UTF-8 path"),
        "--renderer",
        "wgpu",
        "--output",
        dataset.to_str().expect("UTF-8 path"),
    ]);
    assert_eq!(exit_code(&output), 2);
    assert!(stderr(&output).contains("--resume"));
}

#[test]
fn parallel_simulate_writes_the_same_corpus_as_serial_simulate() {
    let root = tempfile::tempdir().expect("temporary root");
    let serial_config = write_run(root.path());
    let config = fs::read_to_string(&serial_config)
        .expect("read run configuration")
        .replace("valid_scene_count = 1", "valid_scene_count = 4");
    fs::write(&serial_config, &config).expect("write serial configuration");
    let parallel_config = root.path().join("parallel.toml");
    fs::write(
        &parallel_config,
        config.replace("simulation_workers = 1", "simulation_workers = 3"),
    )
    .expect("write parallel configuration");

    let mut scenes = Vec::new();
    for (config, corpus) in [(&serial_config, "serial"), (&parallel_config, "parallel")] {
        let corpus = root.path().join(corpus);
        let output = bpp(&[
            "simulate",
            "--config",
            config.to_str().expect("UTF-8 path"),
            "--output",
            corpus.to_str().expect("UTF-8 path"),
        ]);
        assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
        let mut files = fs::read_dir(corpus.join("scenes"))
            .expect("list scenes")
            .map(|entry| entry.expect("scene entry").path())
            .collect::<Vec<_>>();
        files.sort();
        scenes.push(
            files
                .iter()
                .map(|scene| {
                    let name = scene.file_name().expect("scene name").to_owned();
                    let bytes = if scene.is_dir() {
                        fs::read(scene.join("scene.json"))
                    } else {
                        fs::read(scene)
                    };
                    (name, bytes.expect("read scene"))
                })
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(scenes[0].len(), 4);
    assert_eq!(scenes[0], scenes[1]);
}

#[test]
fn seed_and_scene_overrides_are_written_into_the_resolved_config() {
    let root = tempfile::tempdir().expect("temporary root");
    let config = write_run(root.path());
    let corpus = root.path().join("corpus");
    let output = bpp(&[
        "simulate",
        "--config",
        config.to_str().expect("UTF-8 path"),
        "--output",
        corpus.to_str().expect("UTF-8 path"),
        "--seed",
        "1234",
        "--scenes",
        "3",
    ]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report is JSON");
    assert_eq!(report["total_scenes"], 3);
    let resolved =
        fs::read_to_string(corpus.join("resolved-config.toml")).expect("resolved config");
    assert!(resolved.contains("master_seed = 1234"), "{resolved}");
    assert!(resolved.contains("valid_scene_count = 3"), "{resolved}");
}

#[test]
fn json_logging_writes_one_json_object_per_stderr_line() {
    let root = tempfile::tempdir().expect("temporary root");
    let config = write_run(root.path());
    let output = bpp(&[
        "--log-format",
        "json",
        "prepare-assets",
        "--config",
        config.to_str().expect("UTF-8 path"),
    ]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let logs = stderr(&output);
    assert!(!logs.trim().is_empty());
    for line in logs.lines() {
        let event: serde_json::Value = serde_json::from_str(line).expect("log line is JSON");
        assert!(event["fields"]["message"].is_string(), "{line}");
    }
}

#[test]
fn an_unknown_log_format_exits_2() {
    assert_eq!(exit_code(&bpp(&["--log-format", "xml", "schema"])), 2);
}
