use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bpp_core::{SceneId, SceneSnapshot};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::durability::{DurabilityError, DurableBatch, sync_now};

static NEXT_SNAPSHOT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_CORPUS_MANIFEST_TEMP_ID: AtomicU64 = AtomicU64::new(0);
const CORPUS_MANIFEST_SCHEMA_VERSION: u32 = 1;
const CORPUS_CONFIG_DOMAIN: &[u8] = b"bpp.dataset.snapshot-config.v1";

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotCorpusManifest {
    schema_version: u32,
    resolved_config_fingerprint: String,
    asset_catalog_fingerprint: String,
    total_scene_count: u64,
}

/// Creates or validates the immutable identity of a renderer-neutral corpus.
///
/// Exact resolved configuration bytes are persisted beside the manifest. An
/// existing corpus is accepted only when config bytes, asset fingerprint, and
/// scene-ID domain are identical.
///
/// # Errors
///
/// Returns a typed invalid-input, incompatibility, serialization, parse, or
/// filesystem failure before any scene is read or written.
pub fn open_snapshot_corpus(
    corpus_root: &Path,
    resolved_config_bytes: &[u8],
    asset_catalog_fingerprint: &str,
    total_scene_count: u64,
) -> Result<(), SnapshotCorpusOpenError> {
    if asset_catalog_fingerprint.trim().is_empty() {
        return Err(SnapshotCorpusOpenError::BlankAssetCatalogFingerprint);
    }
    if total_scene_count == 0 {
        return Err(SnapshotCorpusOpenError::ZeroScenes);
    }
    fs::create_dir_all(corpus_root).map_err(|source| SnapshotCorpusOpenError::Io {
        path: corpus_root.to_owned(),
        source,
    })?;
    let requested = SnapshotCorpusManifest {
        schema_version: CORPUS_MANIFEST_SCHEMA_VERSION,
        resolved_config_fingerprint: fingerprint_config(resolved_config_bytes),
        asset_catalog_fingerprint: asset_catalog_fingerprint.to_owned(),
        total_scene_count,
    };
    let manifest_path = corpus_root.join("corpus.json");
    let mut manifest_bytes =
        serde_json::to_vec(&requested).map_err(SnapshotCorpusOpenError::SerializeManifest)?;
    manifest_bytes.push(b'\n');
    if !install_file(corpus_root, &manifest_path, &manifest_bytes, "corpus")? {
        let bytes = fs::read(&manifest_path).map_err(|source| SnapshotCorpusOpenError::Io {
            path: manifest_path.clone(),
            source,
        })?;
        let existing: SnapshotCorpusManifest =
            serde_json::from_slice(&bytes).map_err(SnapshotCorpusOpenError::ParseManifest)?;
        if existing != requested {
            return Err(SnapshotCorpusOpenError::IncompatibleManifest);
        }
    }
    let config_path = corpus_root.join("resolved-config.toml");
    if !install_file(
        corpus_root,
        &config_path,
        resolved_config_bytes,
        "resolved-config",
    )? {
        let existing = fs::read(&config_path).map_err(|source| SnapshotCorpusOpenError::Io {
            path: config_path.clone(),
            source,
        })?;
        if existing != resolved_config_bytes {
            return Err(SnapshotCorpusOpenError::IncompatibleResolvedConfig);
        }
    }
    Ok(())
}

/// Validates an existing snapshot corpus without creating or changing files.
///
/// # Errors
///
/// Returns a typed error when the manifest or exact resolved configuration is
/// missing, malformed, unreadable, or incompatible with the requested run.
pub fn validate_snapshot_corpus(
    corpus_root: &Path,
    resolved_config_bytes: &[u8],
    asset_catalog_fingerprint: &str,
    total_scene_count: u64,
) -> Result<(), SnapshotCorpusOpenError> {
    if asset_catalog_fingerprint.trim().is_empty() {
        return Err(SnapshotCorpusOpenError::BlankAssetCatalogFingerprint);
    }
    if total_scene_count == 0 {
        return Err(SnapshotCorpusOpenError::ZeroScenes);
    }
    let requested = SnapshotCorpusManifest {
        schema_version: CORPUS_MANIFEST_SCHEMA_VERSION,
        resolved_config_fingerprint: fingerprint_config(resolved_config_bytes),
        asset_catalog_fingerprint: asset_catalog_fingerprint.to_owned(),
        total_scene_count,
    };
    let manifest_path = corpus_root.join("corpus.json");
    let bytes = fs::read(&manifest_path).map_err(|source| SnapshotCorpusOpenError::Io {
        path: manifest_path,
        source,
    })?;
    let existing: SnapshotCorpusManifest =
        serde_json::from_slice(&bytes).map_err(SnapshotCorpusOpenError::ParseManifest)?;
    if existing != requested {
        return Err(SnapshotCorpusOpenError::IncompatibleManifest);
    }
    let config_path = corpus_root.join("resolved-config.toml");
    let existing_config = fs::read(&config_path).map_err(|source| SnapshotCorpusOpenError::Io {
        path: config_path,
        source,
    })?;
    if existing_config != resolved_config_bytes {
        return Err(SnapshotCorpusOpenError::IncompatibleResolvedConfig);
    }
    Ok(())
}

/// Failure while creating or resuming a snapshot corpus identity.
#[derive(Debug, Error)]
pub enum SnapshotCorpusOpenError {
    /// The immutable asset identity contains no text.
    #[error("asset catalog fingerprint must not be blank")]
    BlankAssetCatalogFingerprint,
    /// A corpus cannot request zero scenes.
    #[error("snapshot corpus scene count must not be zero")]
    ZeroScenes,
    /// A new manifest could not be serialized.
    #[error("snapshot corpus manifest serialization failed: {0}")]
    SerializeManifest(serde_json::Error),
    /// A stored manifest is malformed or has the wrong shape.
    #[error("stored snapshot corpus manifest is invalid: {0}")]
    ParseManifest(serde_json::Error),
    /// Existing immutable corpus identity differs from the requested run.
    #[error("existing snapshot corpus manifest is incompatible with the requested run")]
    IncompatibleManifest,
    /// Existing resolved configuration bytes differ from the requested bytes.
    #[error("resolved configuration is incompatible with the snapshot corpus")]
    IncompatibleResolvedConfig,
    /// Filesystem access failed.
    #[error("snapshot corpus identity I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
}

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
    write_scene_snapshots(corpus_root, std::slice::from_ref(scene))?
        .pop()
        .ok_or_else(|| SnapshotCorpusError::Io {
            path: corpus_root.to_owned(),
            source: io::Error::other("a one-scene batch published nothing"),
        })
}

/// Atomically publishes a batch of scenes with a single storage flush.
///
/// Each scene is still published by its own atomic rename, and every
/// snapshot's data is durable before any rename. A rename becomes durable with
/// the next batch or [`sync_snapshot_index`]; a rename lost to power failure
/// only means that scene is regenerated on resume. Returns the published
/// directories in input order.
///
/// # Errors
///
/// Returns a typed serialization, collision, or filesystem error. Scenes
/// renamed before the error stay published; existing output is never replaced.
pub fn write_scene_snapshots(
    corpus_root: &Path,
    scenes: &[SceneSnapshot],
) -> Result<Vec<PathBuf>, SnapshotCorpusError> {
    let staging_root = corpus_root.join(".staging");
    let scenes_root = corpus_root.join("scenes");
    create_dir_all(&staging_root)?;
    create_dir_all(&scenes_root)?;

    let mut staged = Vec::with_capacity(scenes.len());
    let result = (|| {
        let mut batch = DurableBatch::default();
        for scene in scenes {
            let destination = scenes_root.join(format!("{:020}", scene.scene_id().get()));
            if destination.exists() {
                return Err(SnapshotCorpusError::DestinationExists {
                    scene_id: scene.scene_id(),
                });
            }
            let temporary = create_temporary_directory(&staging_root, scene.scene_id())?;
            staged.push((scene.scene_id(), temporary.clone(), destination));
            let path = temporary.join("scene.json");
            let mut bytes = serde_json::to_vec(scene).map_err(SnapshotCorpusError::Serialize)?;
            bytes.push(b'\n');
            File::create(&path)
                .and_then(|mut file| file.write_all(&bytes))
                .map_err(|source| SnapshotCorpusError::Io {
                    path: path.clone(),
                    source,
                })?;
            batch.add_tree(&temporary).map_err(durability_error)?;
        }
        batch.add(&scenes_root).map_err(durability_error)?;
        batch.flush().map_err(durability_error)?;

        let mut published = Vec::with_capacity(staged.len());
        for (scene_id, temporary, destination) in staged.drain(..) {
            if let Err(source) = fs::rename(&temporary, &destination) {
                let _ = fs::remove_dir_all(&temporary);
                return Err(
                    if source.kind() == io::ErrorKind::AlreadyExists || destination.exists() {
                        SnapshotCorpusError::DestinationExists { scene_id }
                    } else {
                        SnapshotCorpusError::Io {
                            path: destination,
                            source,
                        }
                    },
                );
            }
            published.push(destination);
        }
        DurableBatch::default()
            .add(&scenes_root)
            .map_err(durability_error)?;
        Ok(published)
    })();
    for (_, temporary, _) in staged {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}

/// Makes every published snapshot's rename durable.
///
/// Call once at the end of a run; each batch already makes its own data durable.
///
/// # Errors
///
/// Returns the filesystem error from flushing the corpus scene directory.
pub fn sync_snapshot_index(corpus_root: &Path) -> Result<(), SnapshotCorpusError> {
    sync_now(&corpus_root.join("scenes")).map_err(durability_error)
}

fn durability_error(error: DurabilityError) -> SnapshotCorpusError {
    SnapshotCorpusError::Io {
        path: error.path,
        source: error.source,
    }
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

fn fingerprint_config(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CORPUS_CONFIG_DOMAIN);
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn install_file(
    root: &Path,
    target: &Path,
    bytes: &[u8],
    label: &str,
) -> Result<bool, SnapshotCorpusOpenError> {
    if target.exists() {
        return Ok(false);
    }
    for _ in 0..1024 {
        let nonce = NEXT_CORPUS_MANIFEST_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = root.join(format!(".{label}-{}-{nonce:020}.tmp", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SnapshotCorpusOpenError::Io {
                    path: temporary,
                    source,
                });
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(SnapshotCorpusOpenError::Io {
                path: temporary,
                source,
            });
        }
        let installed = match fs::hard_link(&temporary, target) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(source) => {
                let _ = fs::remove_file(&temporary);
                return Err(SnapshotCorpusOpenError::Io {
                    path: target.to_owned(),
                    source,
                });
            }
        };
        let _ = fs::remove_file(&temporary);
        if installed {
            File::open(root)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| SnapshotCorpusOpenError::Io {
                    path: root.to_owned(),
                    source,
                })?;
        }
        return Ok(installed);
    }
    Err(SnapshotCorpusOpenError::Io {
        path: root.to_owned(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate snapshot manifest staging file",
        ),
    })
}

fn create_dir_all(path: &Path) -> Result<(), SnapshotCorpusError> {
    fs::create_dir_all(path).map_err(|source| SnapshotCorpusError::Io {
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
