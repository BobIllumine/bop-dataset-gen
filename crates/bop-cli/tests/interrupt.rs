//! A real SIGINT stops a run cleanly and the same command resumes it.
#![cfg(unix)]

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const TETRAHEDRON_STL: &str = "solid tetrahedron\n\
    facet normal 0 0 -1\nouter loop\nvertex -1 -1 0\nvertex 0 1 0\nvertex 1 -1 0\nendloop\nendfacet\n\
    facet normal 0 -1 0\nouter loop\nvertex -1 -1 0\nvertex 1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    facet normal 1 1 1\nouter loop\nvertex 1 -1 0\nvertex 0 1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    facet normal -1 1 1\nouter loop\nvertex 0 1 0\nvertex -1 -1 0\nvertex 0 0 1\nendloop\nendfacet\n\
    endsolid tetrahedron\n";

/// Enough scenes that the run is still going when the signal arrives.
const SCENES: u32 = 8;

fn committed_scenes(corpus: &Path) -> usize {
    fs::read_dir(corpus.join("scenes")).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count()
    })
}

fn report(stdout: &[u8]) -> serde_json::Value {
    let line = String::from_utf8_lossy(stdout);
    serde_json::from_str(line.trim()).expect("stdout is one JSON report")
}

#[test]
fn sigint_cancels_simulate_with_status_130_and_the_rerun_resumes() {
    let root = tempfile::tempdir().expect("temporary root");
    fs::write(root.path().join("cube.stl"), TETRAHEDRON_STL).expect("write STL asset");
    let config = include_str!("../../bop-pipeline/tests/fixtures/serial_run.toml")
        .replace("source_path = \"cube.glb\"", "source_path = \"cube.stl\"")
        .replace("source_unit_scale_m = 1.0", "source_unit_scale_m = 0.05")
        .replace(
            "valid_scene_count = 1",
            &format!("valid_scene_count = {SCENES}"),
        );
    let config_path = root.path().join("run.toml");
    fs::write(&config_path, config).expect("write run configuration");
    let corpus = root.path().join("corpus");
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bop-gen"));
        command
            .args(["simulate", "--config"])
            .arg(&config_path)
            .arg("--output")
            .arg(&corpus)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };

    let child = command().spawn().expect("spawn bop-gen simulate");
    let deadline = Instant::now() + Duration::from_mins(1);
    while committed_scenes(&corpus) == 0 {
        assert!(Instant::now() < deadline, "no scene was committed in time");
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());
    let interrupted = child.wait_with_output().expect("wait for bop-gen simulate");

    assert_eq!(
        interrupted.status.code(),
        Some(130),
        "stderr: {}",
        String::from_utf8_lossy(&interrupted.stderr)
    );
    let first = report(&interrupted.stdout);
    assert_eq!(first["cancelled"], true);
    let kept = first["generated_scenes"]
        .as_u64()
        .expect("generated_scenes");
    assert!(kept > 0 && kept < u64::from(SCENES), "kept {kept} scenes");
    assert_eq!(committed_scenes(&corpus), usize::try_from(kept).unwrap());

    let resumed = command().output().expect("rerun bop-gen simulate");
    assert_eq!(resumed.status.code(), Some(0));
    let second = report(&resumed.stdout);
    assert_eq!(second["cancelled"], false);
    assert_eq!(second["preexisting_scenes"].as_u64(), Some(kept));
    assert_eq!(second["total_scenes"].as_u64(), Some(u64::from(SCENES)));
    assert_eq!(committed_scenes(&corpus), SCENES as usize);
}
