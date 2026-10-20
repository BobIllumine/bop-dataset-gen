use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use bop_core::{BackendInfo, SceneId};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const CONFIG_FINGERPRINT_DOMAIN: &[u8] = b"bpp.dataset.resolved-config.v1";
static NEXT_MANIFEST_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Versioned execution environment recorded beside a generated run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunEnvironment {
    schema_version: u16,
    generator_version: String,
    target_os: String,
    target_arch: String,
    build_profile: String,
    enabled_features: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunEnvironmentWire {
    schema_version: u16,
    generator_version: String,
    target_os: String,
    target_arch: String,
    build_profile: String,
    enabled_features: Vec<String>,
}

impl<'de> Deserialize<'de> for RunEnvironment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RunEnvironmentWire::deserialize(deserializer)?;
        if wire.schema_version != 1 {
            return Err(D::Error::custom("unsupported environment schema version"));
        }
        let canonical = Self::try_new(
            wire.generator_version,
            wire.target_os,
            wire.target_arch,
            wire.build_profile,
            wire.enabled_features.clone(),
        )
        .map_err(D::Error::custom)?;
        if canonical.enabled_features != wire.enabled_features {
            return Err(D::Error::custom(
                "environment features must be sorted and unique",
            ));
        }
        Ok(canonical)
    }
}

impl RunEnvironment {
    /// Creates canonical nonblank environment provenance.
    ///
    /// Feature names are sorted and deduplicated for stable serialization.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidRunEnvironment`] if any scalar or feature is blank.
    pub fn try_new(
        generator_version: impl Into<String>,
        target_os: impl Into<String>,
        target_arch: impl Into<String>,
        build_profile: impl Into<String>,
        mut enabled_features: Vec<String>,
    ) -> Result<Self, InvalidRunEnvironment> {
        let generator_version = generator_version.into();
        let target_os = target_os.into();
        let target_arch = target_arch.into();
        let build_profile = build_profile.into();
        for (field, value) in [
            ("generator_version", generator_version.as_str()),
            ("target_os", target_os.as_str()),
            ("target_arch", target_arch.as_str()),
            ("build_profile", build_profile.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(InvalidRunEnvironment::BlankField { field });
            }
        }
        if enabled_features
            .iter()
            .any(|feature| feature.trim().is_empty())
        {
            return Err(InvalidRunEnvironment::BlankFeature);
        }
        enabled_features.sort();
        enabled_features.dedup();
        Ok(Self {
            schema_version: 1,
            generator_version,
            target_os,
            target_arch,
            build_profile,
            enabled_features,
        })
    }
}

/// Invalid environment provenance supplied by a caller.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidRunEnvironment {
    /// One required scalar is blank.
    #[error("environment field {field} must not be blank")]
    BlankField {
        /// Stable field name.
        field: &'static str,
    },
    /// A feature name is blank.
    #[error("environment feature names must not be blank")]
    BlankFeature,
}

/// Atomically persists environment provenance or accepts exact existing bytes.
///
/// # Errors
///
/// Returns a serialization, I/O, or incompatibility error without replacing
/// existing provenance.
pub fn persist_run_environment(
    root: &Path,
    environment: &RunEnvironment,
) -> Result<(), DatasetOpenError> {
    let path = root.join("environment.json");
    let mut bytes = serde_json::to_vec(environment).map_err(DatasetOpenError::SerializeManifest)?;
    bytes.push(b'\n');
    if !install_bytes_atomically(root, &path, &bytes, "environment.json")? {
        let existing = fs::read(&path).map_err(|source| DatasetOpenError::Io {
            path: path.clone(),
            source,
        })?;
        if existing != bytes {
            return Err(DatasetOpenError::IncompatibleEnvironment);
        }
    }
    Ok(())
}

/// Checks existing environment provenance without changing the run directory.
///
/// # Errors
///
/// Returns an I/O, parse, or incompatibility error.
pub fn validate_run_environment(
    root: &Path,
    expected: &RunEnvironment,
) -> Result<(), DatasetOpenError> {
    let path = root.join("environment.json");
    let bytes = fs::read(&path).map_err(|source| DatasetOpenError::Io {
        path: path.clone(),
        source,
    })?;
    let existing: RunEnvironment =
        serde_json::from_slice(&bytes).map_err(DatasetOpenError::ParseEnvironment)?;
    if &existing != expected {
        return Err(DatasetOpenError::IncompatibleEnvironment);
    }
    Ok(())
}

/// Reads checked environment provenance without comparing it to this process.
///
/// This is used when consuming renderer-neutral artifacts: their producing
/// environment remains provenance, but is not a renderer compatibility key.
///
/// # Errors
///
/// Returns an I/O or parse error.
pub fn read_run_environment(root: &Path) -> Result<RunEnvironment, DatasetOpenError> {
    let path = root.join("environment.json");
    let bytes = fs::read(&path).map_err(|source| DatasetOpenError::Io {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(DatasetOpenError::ParseEnvironment)
}

/// Immutable run identity used to reject incompatible dataset resumes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DatasetManifest {
    schema_version: u32,
    resolved_config_fingerprint: String,
    asset_catalog_fingerprint: String,
    backend: BackendInfo,
    total_scene_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetManifestWire {
    schema_version: u32,
    resolved_config_fingerprint: String,
    asset_catalog_fingerprint: String,
    backend: BackendInfo,
    total_scene_count: u64,
}

impl<'de> Deserialize<'de> for DatasetManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = DatasetManifestWire::deserialize(deserializer)?;
        let manifest = Self {
            schema_version: wire.schema_version,
            resolved_config_fingerprint: wire.resolved_config_fingerprint,
            asset_catalog_fingerprint: wire.asset_catalog_fingerprint,
            backend: wire.backend,
            total_scene_count: wire.total_scene_count,
        };
        manifest.validate_loaded().map_err(D::Error::custom)?;
        Ok(manifest)
    }
}

impl DatasetManifest {
    /// Creates a manifest from canonical resolved-config bytes and provenance.
    ///
    /// The caller owns canonical serialization of the resolved configuration;
    /// any byte change intentionally creates an incompatible run identity.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a blank asset fingerprint or zero scenes.
    pub fn try_new(
        resolved_config_bytes: &[u8],
        asset_catalog_fingerprint: impl Into<String>,
        backend: BackendInfo,
        total_scene_count: u64,
    ) -> Result<Self, DatasetManifestError> {
        let asset_catalog_fingerprint = asset_catalog_fingerprint.into();
        if asset_catalog_fingerprint.trim().is_empty() {
            return Err(DatasetManifestError::BlankAssetCatalogFingerprint);
        }
        if total_scene_count == 0 {
            return Err(DatasetManifestError::ZeroScenes);
        }
        Ok(Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            resolved_config_fingerprint: fingerprint_config(resolved_config_bytes),
            asset_catalog_fingerprint,
            backend,
            total_scene_count,
        })
    }

    /// Returns the lowercase hexadecimal resolved-config fingerprint.
    #[must_use]
    pub fn resolved_config_fingerprint(&self) -> &str {
        &self.resolved_config_fingerprint
    }

    /// Returns the immutable resolved asset-catalog fingerprint.
    #[must_use]
    pub fn asset_catalog_fingerprint(&self) -> &str {
        &self.asset_catalog_fingerprint
    }

    /// Returns renderer provenance participating in resume compatibility.
    #[must_use]
    pub const fn backend(&self) -> &BackendInfo {
        &self.backend
    }

    /// Returns the complete scene-ID domain `0..total_scene_count`.
    #[must_use]
    pub const fn total_scene_count(&self) -> u64 {
        self.total_scene_count
    }

    fn validate_loaded(&self) -> Result<(), DatasetManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(DatasetManifestError::UnsupportedSchemaVersion {
                value: self.schema_version,
            });
        }
        if self.resolved_config_fingerprint.len() != 64
            || !self
                .resolved_config_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DatasetManifestError::InvalidResolvedConfigFingerprint);
        }
        if self.asset_catalog_fingerprint.trim().is_empty() {
            return Err(DatasetManifestError::BlankAssetCatalogFingerprint);
        }
        if self.total_scene_count == 0 {
            return Err(DatasetManifestError::ZeroScenes);
        }
        Ok(())
    }
}

/// Why an in-memory or stored dataset manifest is invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DatasetManifestError {
    /// The asset catalog fingerprint contains no non-whitespace text.
    #[error("asset catalog fingerprint must not be blank")]
    BlankAssetCatalogFingerprint,
    /// A run cannot contain zero scenes.
    #[error("total scene count must be nonzero")]
    ZeroScenes,
    /// Stored manifest schema is unsupported.
    #[error("unsupported dataset manifest schema version {value}")]
    UnsupportedSchemaVersion {
        /// Rejected version.
        value: u32,
    },
    /// Stored resolved-config fingerprint is not lowercase BLAKE3 hex.
    #[error("resolved config fingerprint must be 64 lowercase hexadecimal characters")]
    InvalidResolvedConfigFingerprint,
}

/// Sorted view of committed and pending scenes for a compatible run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatasetResumeIndex {
    manifest: DatasetManifest,
    committed_scenes: Vec<SceneId>,
}

impl DatasetResumeIndex {
    /// Creates a new run manifest or resumes an exactly compatible one.
    ///
    /// Only canonical twenty-digit directories below `scenes/` count as
    /// committed. Private `.staging` contents never participate in resume.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, manifest, compatibility, or scene-directory error.
    pub fn open_or_create(
        dataset_root: &Path,
        requested: DatasetManifest,
    ) -> Result<Self, DatasetOpenError> {
        requested.validate_loaded()?;
        fs::create_dir_all(dataset_root).map_err(|source| DatasetOpenError::Io {
            path: dataset_root.to_owned(),
            source,
        })?;
        let manifest_path = dataset_root.join("run.json");
        let manifest = if install_manifest_atomically(dataset_root, &manifest_path, &requested)? {
            requested
        } else {
            let bytes = fs::read(&manifest_path).map_err(|source| DatasetOpenError::Io {
                path: manifest_path.clone(),
                source,
            })?;
            let existing: DatasetManifest =
                serde_json::from_slice(&bytes).map_err(DatasetOpenError::ParseManifest)?;
            existing.validate_loaded()?;
            if existing != requested {
                return Err(DatasetOpenError::IncompatibleManifest {
                    existing: Box::new(existing),
                    requested: Box::new(requested),
                });
            }
            existing
        };

        let scenes_path = dataset_root.join("scenes");
        fs::create_dir_all(&scenes_path).map_err(|source| DatasetOpenError::Io {
            path: scenes_path.clone(),
            source,
        })?;
        let mut committed_scenes = Vec::new();
        let entries = fs::read_dir(&scenes_path).map_err(|source| DatasetOpenError::Io {
            path: scenes_path.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| DatasetOpenError::Io {
                path: scenes_path.clone(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| DatasetOpenError::Io {
                path: entry.path(),
                source,
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| DatasetOpenError::InvalidSceneDirectory { path: entry.path() })?;
            let value = parse_scene_directory(name, manifest.total_scene_count)
                .ok_or_else(|| DatasetOpenError::InvalidSceneDirectory { path: entry.path() })?;
            committed_scenes.push(SceneId::new(value));
        }
        committed_scenes.sort_unstable();
        Ok(Self {
            manifest,
            committed_scenes,
        })
    }

    /// Returns immutable run identity and provenance.
    #[must_use]
    pub const fn manifest(&self) -> &DatasetManifest {
        &self.manifest
    }

    /// Returns committed scenes in ascending identifier order.
    #[must_use]
    pub fn committed_scenes(&self) -> &[SceneId] {
        &self.committed_scenes
    }

    /// Iterates missing scene identifiers in ascending order without allocation.
    pub fn pending_scenes(&self) -> impl Iterator<Item = SceneId> + '_ {
        (0..self.manifest.total_scene_count).filter_map(|value| {
            let scene_id = SceneId::new(value);
            self.committed_scenes
                .binary_search(&scene_id)
                .is_err()
                .then_some(scene_id)
        })
    }

    /// Atomically persists the exact resolved configuration used by this run.
    ///
    /// Existing bytes must match exactly. The manifest fingerprint is checked
    /// before any write so callers cannot publish config bytes for another run.
    ///
    /// # Errors
    ///
    /// Returns a typed compatibility or filesystem failure.
    pub fn persist_resolved_config(
        &self,
        dataset_root: &Path,
        resolved_config_bytes: &[u8],
    ) -> Result<(), DatasetOpenError> {
        if fingerprint_config(resolved_config_bytes) != self.manifest.resolved_config_fingerprint {
            return Err(DatasetOpenError::IncompatibleResolvedConfig);
        }
        let path = dataset_root.join("resolved-config.toml");
        if !install_bytes_atomically(
            dataset_root,
            &path,
            resolved_config_bytes,
            "resolved-config.toml",
        )? {
            let existing = fs::read(&path).map_err(|source| DatasetOpenError::Io {
                path: path.clone(),
                source,
            })?;
            if existing != resolved_config_bytes {
                return Err(DatasetOpenError::IncompatibleResolvedConfig);
            }
        }
        Ok(())
    }
}

/// Failure to initialize or resume a dataset run.
#[derive(Debug, Error)]
pub enum DatasetOpenError {
    /// Filesystem access failed.
    #[error("dataset I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A new manifest could not be serialized.
    #[error("dataset manifest serialization failed: {0}")]
    SerializeManifest(serde_json::Error),
    /// A stored manifest is malformed JSON or has the wrong shape.
    #[error("stored dataset manifest is invalid: {0}")]
    ParseManifest(serde_json::Error),
    /// Stored environment JSON is malformed or noncanonical.
    #[error("stored environment provenance is invalid: {0}")]
    ParseEnvironment(serde_json::Error),
    /// A stored manifest violates checked field invariants.
    #[error(transparent)]
    InvalidManifest(#[from] DatasetManifestError),
    /// Existing immutable run identity differs from the requested run.
    #[error("existing dataset manifest is incompatible with the requested run")]
    IncompatibleManifest {
        /// Existing stored identity.
        existing: Box<DatasetManifest>,
        /// Newly requested identity.
        requested: Box<DatasetManifest>,
    },
    /// A committed-scene directory name is malformed or outside the run domain.
    #[error("invalid committed scene directory {path}")]
    InvalidSceneDirectory {
        /// Rejected directory path.
        path: PathBuf,
    },
    /// Stored or supplied resolved configuration bytes contradict the manifest.
    #[error("resolved configuration bytes are incompatible with the dataset manifest")]
    IncompatibleResolvedConfig,
    /// Stored execution environment differs from this invocation.
    #[error("execution environment is incompatible with the stored run")]
    IncompatibleEnvironment,
    /// A stored canonical model differs from the requested asset catalog.
    #[error("stored model {path} differs from the requested asset catalog")]
    IncompatibleAssets {
        /// Stored file that differs.
        path: PathBuf,
    },
    /// An asset's visual mesh cannot be stored as a canonical model.
    #[error("asset mesh cannot be stored: {0}")]
    InvalidModel(#[from] crate::InvalidCanonicalModel),
}

/// Returns the fingerprint a dataset manifest records for resolved configuration bytes.
#[must_use]
pub fn fingerprint_config(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONFIG_FINGERPRINT_DOMAIN);
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn install_manifest_atomically(
    dataset_root: &Path,
    manifest_path: &Path,
    manifest: &DatasetManifest,
) -> Result<bool, DatasetOpenError> {
    let mut bytes = serde_json::to_vec(manifest).map_err(DatasetOpenError::SerializeManifest)?;
    bytes.push(b'\n');
    install_bytes_atomically(dataset_root, manifest_path, &bytes, "run.json")
}

pub(crate) fn install_bytes_atomically(
    dataset_root: &Path,
    target: &Path,
    bytes: &[u8],
    temporary_label: &str,
) -> Result<bool, DatasetOpenError> {
    if target.exists() {
        return Ok(false);
    }
    for _ in 0..1024 {
        let nonce = NEXT_MANIFEST_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = dataset_root.join(format!(
            ".{temporary_label}-{}-{nonce:020}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(DatasetOpenError::Io {
                    path: temporary,
                    source,
                });
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(DatasetOpenError::Io {
                path: temporary,
                source,
            });
        }
        let installed = match fs::hard_link(&temporary, target) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(source) => {
                let _ = fs::remove_file(&temporary);
                return Err(DatasetOpenError::Io {
                    path: target.to_owned(),
                    source,
                });
            }
        };
        let _ = fs::remove_file(&temporary);
        if installed {
            sync_directory(dataset_root)?;
        }
        return Ok(installed);
    }
    Err(DatasetOpenError::Io {
        path: dataset_root.to_owned(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique manifest staging file after 1024 attempts",
        ),
    })
}

fn parse_scene_directory(name: &str, total_scene_count: u64) -> Option<u64> {
    if name.len() != 20 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = name.parse::<u64>().ok()?;
    (value < total_scene_count && name == format!("{value:020}")).then_some(value)
}

fn sync_directory(path: &Path) -> Result<(), DatasetOpenError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| DatasetOpenError::Io {
            path: path.to_owned(),
            source,
        })
}
