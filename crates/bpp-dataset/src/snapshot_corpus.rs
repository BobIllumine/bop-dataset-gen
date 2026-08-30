use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bpp_core::{SceneId, SceneSnapshot};
use thiserror::Error;

static NEXT_SNAPSHOT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Atomically publishes one canonical physical scene for a later render-only pass.
///
/// # Errors
///
/// Returns a typed serialization, collision, or filesystem error. Existing
/// scene output is never replaced.
pub fn write_scene_snapshot(
    corpus_root: &Path,
    scene: &SceneSnapshot,
) -> Result<PathBuf, SnapshotCorpusError> {
    let staging_root = corpus_root.join(".staging");
    let scenes_root = corpus_root.join("scenes");
    create_dir_all(&staging_root)?;
    create_dir_all(&scenes_root)?;
    let destination = scenes_root.join(format!("{:020}", scene.scene_id().get()));
    if destination.exists() {
        return Err(SnapshotCorpusError::DestinationExists {
            scene_id: scene.scene_id(),
        });
    }

    let temporary = create_temporary_directory(&staging_root, scene.scene_id())?;
    let result = (|| {
        let path = temporary.join("scene.json");
        let mut bytes = serde_json::to_vec(scene).map_err(SnapshotCorpusError::Serialize)?;
        bytes.push(b'\n');
        let mut file = File::create(&path).map_err(|source| SnapshotCorpusError::Io {
            path: path.clone(),
            source,
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| SnapshotCorpusError::Io {
                path: path.clone(),
                source,
            })?;
        sync_directory(&temporary)?;
        fs::rename(&temporary, &destination).map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists || destination.exists() {
                SnapshotCorpusError::DestinationExists {
                    scene_id: scene.scene_id(),
                }
            } else {
                SnapshotCorpusError::Io {
                    path: destination.clone(),
                    source,
                }
            }
        })?;
        sync_directory(&scenes_root)?;
        Ok(destination.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

/// Reads a snapshot corpus in ascending scene-ID order.
///
/// # Errors
///
/// Returns a typed directory, I/O, JSON, or embedded-identity error.
pub fn read_snapshot_corpus(corpus_root: &Path) -> Result<Vec<SceneSnapshot>, SnapshotCorpusError> {
    let scenes_root = corpus_root.join("scenes");
    let entries = fs::read_dir(&scenes_root).map_err(|source| SnapshotCorpusError::Io {
        path: scenes_root.clone(),
        source,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SnapshotCorpusError::Io {
            path: scenes_root.clone(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| SnapshotCorpusError::Io {
                path: entry.path(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| SnapshotCorpusError::InvalidSceneDirectory { path: path.clone() })?;
        let scene_id = parse_scene_id(&name)
            .ok_or_else(|| SnapshotCorpusError::InvalidSceneDirectory { path: path.clone() })?;
        paths.push((scene_id, path));
    }
    paths.sort_unstable_by_key(|(scene_id, _)| *scene_id);

    paths
        .into_iter()
        .map(|(expected_id, directory)| {
            let path = directory.join("scene.json");
            let bytes = fs::read(&path).map_err(|source| SnapshotCorpusError::Io {
                path: path.clone(),
                source,
            })?;
            let scene: SceneSnapshot =
                serde_json::from_slice(&bytes).map_err(|source| SnapshotCorpusError::Parse {
                    path: path.clone(),
                    source,
                })?;
            if scene.scene_id() != expected_id {
                return Err(SnapshotCorpusError::SceneIdMismatch {
                    path,
                    directory_scene_id: expected_id,
                    snapshot_scene_id: scene.scene_id(),
                });
            }
            Ok(scene)
        })
        .collect()
}

/// Failure while publishing or reading a simulation snapshot corpus.
#[derive(Debug, Error)]
pub enum SnapshotCorpusError {
    /// A scene destination already exists and was protected from replacement.
    #[error("snapshot scene {scene_id:?} already exists")]
    DestinationExists {
        /// Existing scene identity.
        scene_id: SceneId,
    },
    /// A scene directory does not use the canonical twenty-digit identity.
    #[error("invalid snapshot scene directory {path}")]
    InvalidSceneDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// Directory identity and embedded scene identity disagree.
    #[error(
        "snapshot {path} contains scene {snapshot_scene_id:?}, expected {directory_scene_id:?}"
    )]
    SceneIdMismatch {
        /// Snapshot metadata path.
        path: PathBuf,
        /// Identity encoded by the directory.
        directory_scene_id: SceneId,
        /// Identity encoded by the DTO.
        snapshot_scene_id: SceneId,
    },
    /// Canonical JSON serialization failed.
    #[error("snapshot serialization failed: {0}")]
    Serialize(serde_json::Error),
    /// Stored canonical JSON could not be decoded.
    #[error("snapshot {path} is invalid: {source}")]
    Parse {
        /// Invalid metadata path.
        path: PathBuf,
        /// JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// Filesystem access failed.
    #[error("snapshot corpus I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
}

fn create_temporary_directory(
    staging_root: &Path,
    scene_id: SceneId,
) -> Result<PathBuf, SnapshotCorpusError> {
    for _ in 0..1024 {
        let nonce = NEXT_SNAPSHOT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = staging_root.join(format!(
            "snapshot-{:020}-{}-{nonce:020}",
            scene_id.get(),
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(SnapshotCorpusError::Io { path, source }),
        }
    }
    Err(SnapshotCorpusError::Io {
        path: staging_root.to_owned(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate snapshot staging directory",
        ),
    })
}

fn create_dir_all(path: &Path) -> Result<(), SnapshotCorpusError> {
    fs::create_dir_all(path).map_err(|source| SnapshotCorpusError::Io {
        path: path.to_owned(),
        source,
    })
}

fn sync_directory(path: &Path) -> Result<(), SnapshotCorpusError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| SnapshotCorpusError::Io {
            path: path.to_owned(),
            source,
        })
}

fn parse_scene_id(name: &str) -> Option<SceneId> {
    if name.len() != 20 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = name.parse::<u64>().ok()?;
    (format!("{value:020}") == name).then_some(SceneId::new(value))
}
