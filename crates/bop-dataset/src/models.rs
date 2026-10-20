//! Canonical model meshes stored with a dataset.
//!
//! Each asset is stored under `assets/<encoded asset id>/` as `asset.json` and
//! `model.ply`. The mesh is the renderer-neutral visual geometry in canonical
//! model-frame metres, so instance poses in `scene.json` place it exactly.
//! Exporters read the models from here instead of re-resolving the original
//! source files.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use bop_core::AssetId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DatasetOpenError, resume::install_bytes_atomically};

const PLY_HEADER_PREFIX: &str =
    "ply\nformat binary_little_endian 1.0\ncomment bop canonical model\n";

/// Triangle mesh of one asset in canonical model-frame metres.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalModel {
    asset_id: AssetId,
    content_fingerprint: String,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl CanonicalModel {
    /// Creates a checked model mesh.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidCanonicalModel`] for empty, non-finite, mismatched, or
    /// out-of-range geometry.
    pub fn try_new(
        asset_id: AssetId,
        content_fingerprint: String,
        positions: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        indices: Vec<u32>,
    ) -> Result<Self, InvalidCanonicalModel> {
        if positions.is_empty() || indices.is_empty() {
            return Err(InvalidCanonicalModel::Empty);
        }
        if normals.len() != positions.len() {
            return Err(InvalidCanonicalModel::NormalCount);
        }
        if !indices.len().is_multiple_of(3) {
            return Err(InvalidCanonicalModel::PartialTriangle);
        }
        if positions
            .iter()
            .chain(&normals)
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(InvalidCanonicalModel::NonFinite);
        }
        let vertex_count =
            u32::try_from(positions.len()).map_err(|_| InvalidCanonicalModel::TooLarge)?;
        if indices.iter().any(|index| *index >= vertex_count) {
            return Err(InvalidCanonicalModel::IndexOutOfRange);
        }
        Ok(Self {
            asset_id,
            content_fingerprint,
            positions,
            normals,
            indices,
        })
    }

    /// Returns the logical asset identity.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    /// Returns the asset's preprocessing content fingerprint.
    #[must_use]
    pub fn content_fingerprint(&self) -> &str {
        &self.content_fingerprint
    }

    /// Returns vertex positions in model-frame metres.
    #[must_use]
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Returns unit vertex normals in the model frame.
    #[must_use]
    pub fn normals(&self) -> &[[f32; 3]] {
        &self.normals
    }

    /// Returns triangle-list vertex indices.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Encodes the mesh as binary PLY with positions multiplied by `scale`.
    pub(crate) fn to_ply(&self, scale: f32) -> Vec<u8> {
        let triangles = self.indices.len() / 3;
        let mut bytes = format!(
            "{PLY_HEADER_PREFIX}element vertex {}\n\
             property float x\nproperty float y\nproperty float z\n\
             property float nx\nproperty float ny\nproperty float nz\n\
             element face {triangles}\nproperty list uchar uint vertex_indices\nend_header\n",
            self.positions.len()
        )
        .into_bytes();
        bytes.reserve(self.positions.len() * 24 + triangles * 13);
        for (position, normal) in self.positions.iter().zip(&self.normals) {
            for value in position.map(|value| value * scale).iter().chain(normal) {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        for triangle in self.indices.chunks_exact(3) {
            bytes.push(3);
            for index in triangle {
                bytes.extend_from_slice(&index.to_le_bytes());
            }
        }
        bytes
    }

    fn from_ply(
        asset_id: AssetId,
        content_fingerprint: String,
        bytes: &[u8],
    ) -> Result<Self, InvalidCanonicalModel> {
        let malformed = || InvalidCanonicalModel::MalformedPly;
        let header_end = bytes
            .windows(11)
            .position(|window| window == b"end_header\n")
            .ok_or_else(malformed)?
            + 11;
        let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| malformed())?;
        let count = |element: &str| -> Result<usize, InvalidCanonicalModel> {
            header
                .lines()
                .find_map(|line| line.strip_prefix(element))
                .and_then(|value| value.parse().ok())
                .ok_or_else(malformed)
        };
        let vertices = count("element vertex ")?;
        let faces = count("element face ")?;
        let expected = Self {
            asset_id: asset_id.clone(),
            content_fingerprint: content_fingerprint.clone(),
            positions: vec![[0.0; 3]; vertices],
            normals: vec![[0.0; 3]; vertices],
            indices: vec![0; faces * 3],
        };
        if !expected.to_ply(1.0).starts_with(&bytes[..header_end]) {
            return Err(malformed());
        }
        let body = &bytes[header_end..];
        if body.len() != vertices * 24 + faces * 13 {
            return Err(malformed());
        }
        let float = |offset: usize| {
            f32::from_le_bytes(body[offset..offset + 4].try_into().expect("four bytes"))
        };
        let mut positions = Vec::with_capacity(vertices);
        let mut normals = Vec::with_capacity(vertices);
        for vertex in 0..vertices {
            let base = vertex * 24;
            positions.push([float(base), float(base + 4), float(base + 8)]);
            normals.push([float(base + 12), float(base + 16), float(base + 20)]);
        }
        let mut indices = Vec::with_capacity(faces * 3);
        for face in body[vertices * 24..].chunks_exact(13) {
            if face[0] != 3 {
                return Err(malformed());
            }
            for corner in face[1..].chunks_exact(4) {
                indices.push(u32::from_le_bytes(corner.try_into().expect("four bytes")));
            }
        }
        Self::try_new(asset_id, content_fingerprint, positions, normals, indices)
    }
}

/// A model mesh violates basic integrity rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum InvalidCanonicalModel {
    /// The mesh has no vertices or no triangles.
    #[error("model mesh is empty")]
    Empty,
    /// Normal and position counts differ.
    #[error("model mesh has a different number of normals and positions")]
    NormalCount,
    /// The index count is not a multiple of three.
    #[error("model mesh index count is not a whole number of triangles")]
    PartialTriangle,
    /// A position or normal is not finite.
    #[error("model mesh contains a non-finite value")]
    NonFinite,
    /// A triangle references a missing vertex.
    #[error("model mesh index is out of range")]
    IndexOutOfRange,
    /// The vertex count does not fit the PLY index type.
    #[error("model mesh has too many vertices")]
    TooLarge,
    /// A stored PLY file is not in the canonical layout.
    #[error("stored model PLY is malformed")]
    MalformedPly,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AssetRecord {
    schema_version: u16,
    asset_id: AssetId,
    content_fingerprint: String,
    model: String,
    model_blake3: String,
    vertex_count: usize,
    triangle_count: usize,
}

/// Returns the portable directory name for an asset.
///
/// Bytes outside `[A-Za-z0-9_-]` and a leading `.` are percent-encoded, so
/// every asset ID maps to a distinct, safe file name.
fn asset_directory_name(asset_id: &AssetId) -> String {
    let mut name = String::new();
    for (position, byte) in asset_id.as_str().bytes().enumerate() {
        if byte.is_ascii_alphanumeric()
            || byte == b'_'
            || byte == b'-'
            || (byte == b'.' && position > 0)
        {
            name.push(char::from(byte));
        } else {
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name
}

/// Stores every model under `assets/`, or checks that stored models match.
///
/// Files are installed atomically and never replaced. A resumed run must
/// present byte-identical models.
///
/// # Errors
///
/// Returns a filesystem failure, or [`DatasetOpenError::IncompatibleAssets`]
/// when a stored model differs from the requested one.
pub fn persist_canonical_models(
    dataset_root: &Path,
    models: &[CanonicalModel],
) -> Result<(), DatasetOpenError> {
    for model in models {
        let directory = dataset_root
            .join("assets")
            .join(asset_directory_name(model.asset_id()));
        fs::create_dir_all(&directory).map_err(|source| DatasetOpenError::Io {
            path: directory.clone(),
            source,
        })?;
        let mesh = model.to_ply(1.0);
        let record = AssetRecord {
            schema_version: 1,
            asset_id: model.asset_id().clone(),
            content_fingerprint: model.content_fingerprint().to_owned(),
            model: "model.ply".to_owned(),
            model_blake3: blake3::hash(&mesh).to_hex().to_string(),
            vertex_count: model.positions().len(),
            triangle_count: model.indices().len() / 3,
        };
        let mut record =
            serde_json::to_vec_pretty(&record).map_err(DatasetOpenError::SerializeManifest)?;
        record.push(b'\n');
        for (name, bytes) in [
            ("model.ply", mesh.as_slice()),
            ("asset.json", record.as_slice()),
        ] {
            let path = directory.join(name);
            if !install_bytes_atomically(dataset_root, &path, bytes, name)? {
                let existing = fs::read(&path).map_err(|source| DatasetOpenError::Io {
                    path: path.clone(),
                    source,
                })?;
                if existing != bytes {
                    return Err(DatasetOpenError::IncompatibleAssets { path });
                }
            }
        }
    }
    Ok(())
}

/// Reads every stored model, sorted by asset ID.
///
/// # Errors
///
/// Returns [`CanonicalModelError`] for missing, malformed, or tampered files.
pub fn read_canonical_models(
    dataset_root: &Path,
) -> Result<Vec<CanonicalModel>, CanonicalModelError> {
    let root = dataset_root.join("assets");
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| CanonicalModelError::Io { path, source }
    };
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in fs::read_dir(&root).map_err(io(&root))? {
        let entry = entry.map_err(io(&root))?;
        let directory = entry.path();
        let record_path = directory.join("asset.json");
        let record: AssetRecord = serde_json::from_slice(
            &fs::read(&record_path).map_err(io(&record_path))?,
        )
        .map_err(|source| CanonicalModelError::Record {
            path: record_path.clone(),
            source,
        })?;
        let invalid = |path: PathBuf| CanonicalModelError::Invalid { path };
        if record.schema_version != 1
            || record.model != "model.ply"
            || entry.file_name().to_str() != Some(asset_directory_name(&record.asset_id).as_str())
            || !seen.insert(record.asset_id.clone())
        {
            return Err(invalid(record_path));
        }
        let mesh_path = directory.join(&record.model);
        let mesh = fs::read(&mesh_path).map_err(io(&mesh_path))?;
        if blake3::hash(&mesh).to_hex().as_str() != record.model_blake3 {
            return Err(invalid(mesh_path));
        }
        let model = CanonicalModel::from_ply(record.asset_id, record.content_fingerprint, &mesh)
            .map_err(|_| invalid(mesh_path.clone()))?;
        if model.positions().len() != record.vertex_count
            || model.indices().len() / 3 != record.triangle_count
        {
            return Err(invalid(mesh_path));
        }
        models.push(model);
    }
    if models.is_empty() {
        return Err(CanonicalModelError::Missing { path: root });
    }
    models.sort_by(|left, right| left.asset_id().cmp(right.asset_id()));
    Ok(models)
}

/// Stored canonical models cannot be read.
#[derive(Debug, Error)]
pub enum CanonicalModelError {
    /// Filesystem access failed.
    #[error("model I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// An `asset.json` record is malformed.
    #[error("asset record {path} is invalid: {source}")]
    Record {
        /// Rejected record.
        path: PathBuf,
        /// Parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// A record or mesh is inconsistent or was modified.
    #[error("stored model {path} is inconsistent or was modified")]
    Invalid {
        /// Rejected file.
        path: PathBuf,
    },
    /// The dataset stores no models; it predates model storage.
    #[error("dataset has no stored models under {path}; regenerate it with this version")]
    Missing {
        /// Expected models directory.
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetrahedron() -> CanonicalModel {
        CanonicalModel::try_new(
            AssetId::new("part/a").expect("asset id"),
            "fingerprint".to_owned(),
            vec![
                [0.0, 0.0, 0.0],
                [0.01, 0.0, 0.0],
                [0.0, 0.01, 0.0],
                [0.0, 0.0, 0.01],
            ],
            vec![[0.0, 0.0, 1.0]; 4],
            vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
        )
        .expect("valid model")
    }

    #[test]
    fn directory_names_are_portable_and_distinct() {
        let name = |id: &str| asset_directory_name(&AssetId::new(id).expect("asset id"));
        assert_eq!(name("cube_v2.1"), "cube_v2.1");
        assert_eq!(name("../x"), "%2E.%2Fx");
        assert_eq!(name(".hidden"), "%2Ehidden");
        assert_ne!(name("a/b"), name("a%2Fb"));
    }

    #[test]
    fn stored_models_round_trip_and_reject_changes() {
        let root = tempfile::tempdir().expect("temporary dataset");
        let model = tetrahedron();
        persist_canonical_models(root.path(), std::slice::from_ref(&model)).expect("persist");
        persist_canonical_models(root.path(), std::slice::from_ref(&model)).expect("idempotent");
        assert_eq!(
            read_canonical_models(root.path()).expect("read"),
            vec![model.clone()]
        );

        let mut moved = model.positions().to_vec();
        moved[0][0] = 0.001;
        let changed = CanonicalModel::try_new(
            model.asset_id().clone(),
            model.content_fingerprint().to_owned(),
            moved,
            model.normals().to_vec(),
            model.indices().to_vec(),
        )
        .expect("valid model");
        assert!(matches!(
            persist_canonical_models(root.path(), &[changed]),
            Err(DatasetOpenError::IncompatibleAssets { .. })
        ));

        let mesh = root.path().join("assets/part%2Fa/model.ply");
        let mut bytes = fs::read(&mesh).expect("read mesh");
        *bytes.last_mut().expect("nonempty") ^= 1;
        fs::write(&mesh, bytes).expect("tamper");
        assert!(matches!(
            read_canonical_models(root.path()),
            Err(CanonicalModelError::Invalid { .. })
        ));
    }

    #[test]
    fn rejects_out_of_range_indices() {
        assert_eq!(
            CanonicalModel::try_new(
                AssetId::new("a").expect("asset id"),
                String::new(),
                vec![[0.0; 3]; 3],
                vec![[0.0; 3]; 3],
                vec![0, 1, 3],
            ),
            Err(InvalidCanonicalModel::IndexOutOfRange)
        );
    }
}
