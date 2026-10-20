use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use bop_assets::{
    AssetCacheKeyInput, AssetSourceFormat, CanonicalizeVisualAssetError, ConvexDecompositionCache,
    ConvexDecompositionCacheError, GlbLoadError, GlbLoadLimits, InvalidNormalizedAssetError,
    LoadedVisualAsset, MeshOptimizationError, MeshOptimizationOptions, NormalizedAsset,
    OptimizedAsset, ResolvedAsset, ResolvedAssetCatalog, ResolvedAssetCatalogError, StlLoadError,
    StlLoadOptions, asset_cache_key, canonicalize_visual_asset, generate_convex_parts, load_glb,
    load_stl, optimize_visual_asset,
};
use bop_core::{AssetId, RunPlan};
use bop_physics_rapier::{PhysicsAssetPreparationError, PreparedPhysicsAsset};
use thiserror::Error;

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STL_TRIANGLES: usize = 1_300_000;

/// Renderer and physics views prepared from the same canonical asset geometry.
pub struct PreparedAssetCatalog {
    visual_assets: ResolvedAssetCatalog,
    physics_assets: BTreeMap<AssetId, Arc<PreparedPhysicsAsset>>,
}

impl PreparedAssetCatalog {
    /// Borrows the immutable renderer catalog.
    #[must_use]
    pub const fn visual_assets(&self) -> &ResolvedAssetCatalog {
        &self.visual_assets
    }

    /// Borrows collision assets in stable asset-ID order.
    #[must_use]
    pub const fn physics_assets(&self) -> &BTreeMap<AssetId, Arc<PreparedPhysicsAsset>> {
        &self.physics_assets
    }

    /// Clones only the cheap shared pointers required by a physics generator.
    #[must_use]
    pub fn shared_physics_assets(&self) -> BTreeMap<AssetId, Arc<PreparedPhysicsAsset>> {
        self.physics_assets.clone()
    }
}

/// Loads, canonicalizes, decomposes, and prepares every configured asset.
///
/// Relative source and cache paths are resolved against `config_directory`.
/// Source reads are limited to 64 MiB. Convex decomposition uses the existing
/// content-addressed cache, and both returned catalogs share its checked parts.
///
/// # Errors
///
/// Returns a typed path, format, loading, canonicalization, cache, normalized
/// asset, catalog, or Rapier preparation error.
pub fn prepare_asset_catalog(
    plan: &RunPlan,
    config_directory: &Path,
) -> Result<PreparedAssetCatalog, AssetPreparationError> {
    let cache = ConvexDecompositionCache::new(resolve(config_directory, plan.run().cache_path()));
    let mut visual_entries = Vec::with_capacity(plan.assets().len());
    let mut physics_assets = BTreeMap::new();

    for configured in plan.assets() {
        let source_path = resolve(config_directory, configured.source_path());
        let format = source_format(&source_path)?;
        let source_bytes = read_bounded(&source_path)?;
        let key = asset_cache_key(AssetCacheKeyInput::new(
            &source_bytes,
            format,
            configured.source_unit_scale_m(),
            configured.model_transform(),
            configured.vhacd(),
        ));
        let loaded = load_source::<AssetPreparationError>(
            &source_bytes,
            format,
            configured.source_unit_scale_m().get(),
        )?;
        let canonical = canonicalize_visual_asset(loaded, configured.model_transform())?;
        let cached = cache.get_or_generate(key, || {
            generate_convex_parts(&canonical, configured.vhacd())
        })?;
        let normalized = Arc::new(NormalizedAsset::try_new(
            canonical.visual_mesh().clone(),
            canonical.materials().to_vec(),
            canonical.textures().to_vec(),
            cached.parts().to_vec(),
        )?);
        let physics = Arc::new(PreparedPhysicsAsset::try_new(normalized.convex_parts())?);
        physics_assets.insert(configured.id().clone(), physics);
        visual_entries.push(ResolvedAsset::new(
            configured.id().clone(),
            *key.as_bytes(),
            normalized,
        ));
    }

    let mut visual_assets = ResolvedAssetCatalog::try_new(visual_entries)?;
    if let Some(path) = plan.appearance().environment_path() {
        let path = resolve(config_directory, path);
        let bytes = read_bounded(&path)?;
        let environment = bop_assets::load_environment(&path, &bytes)
            .map_err(|source| AssetPreparationError::Environment { path, source })?;
        visual_assets = visual_assets.with_environment(Arc::new(environment));
    }
    Ok(PreparedAssetCatalog {
        visual_assets,
        physics_assets,
    })
}

fn resolve(base: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_owned()
    } else {
        base.join(configured)
    }
}

fn source_format(path: &Path) -> Result<AssetSourceFormat, AssetPreparationError> {
    match path.extension().and_then(|value| value.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("stl") => Ok(AssetSourceFormat::Stl),
        Some(extension) if extension.eq_ignore_ascii_case("glb") => Ok(AssetSourceFormat::Glb),
        _ => Err(AssetPreparationError::UnsupportedFormat {
            path: path.to_owned(),
        }),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, AssetPreparationError> {
    let file = File::open(path).map_err(|source| AssetPreparationError::Read {
        path: path.to_owned(),
        source,
    })?;
    let declared = file
        .metadata()
        .map_err(|source| AssetPreparationError::Read {
            path: path.to_owned(),
            source,
        })?
        .len();
    if declared > MAX_SOURCE_BYTES {
        return Err(AssetPreparationError::SourceTooLarge {
            path: path.to_owned(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    file.take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| AssetPreparationError::Read {
            path: path.to_owned(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SOURCE_BYTES {
        return Err(AssetPreparationError::SourceTooLarge {
            path: path.to_owned(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    Ok(bytes)
}

/// Loads source bytes; `source_unit_scale_m` applies to STL only, as GLB is
/// metres by definition.
fn load_source<E: From<StlLoadError> + From<GlbLoadError>>(
    bytes: &[u8],
    format: AssetSourceFormat,
    source_unit_scale_m: f32,
) -> Result<LoadedVisualAsset, E> {
    match format {
        AssetSourceFormat::Stl => {
            let options = StlLoadOptions::try_new(source_unit_scale_m, MAX_STL_TRIANGLES)
                .expect("validated scale and positive fixed limit");
            Ok(LoadedVisualAsset::from_unmaterialized_mesh(load_stl(
                bytes, options,
            )?))
        }
        AssetSourceFormat::Glb => Ok(load_glb(bytes, glb_limits())?),
    }
}

/// Why a library file cannot be optimized.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetOptimizationError {
    /// The file name does not end in `.stl` or `.glb`.
    #[error("only .stl and .glb files can be optimized")]
    UnsupportedFormat,
    /// The STL unit scale is not positive and finite.
    #[error("source_unit_scale_m must be positive and finite")]
    InvalidUnitScale,
    /// The source exceeds the ingestion bound.
    #[error("the source exceeds {MAX_SOURCE_BYTES} bytes")]
    SourceTooLarge,
    /// STL ingestion failed.
    #[error(transparent)]
    Stl(#[from] StlLoadError),
    /// GLB ingestion failed.
    #[error(transparent)]
    Glb(#[from] GlbLoadError),
    /// Optimization failed or could not meet the budget.
    #[error(transparent)]
    Optimize(#[from] MeshOptimizationError),
}

/// Optimizes one STL or GLB source, loaded exactly as generation loads it,
/// into a GLB in metres. `file_name` selects the format; `source_unit_scale_m`
/// applies to STL only.
///
/// # Errors
///
/// Returns [`AssetOptimizationError`] for an unsupported or unreadable
/// source, or when optimization fails.
pub fn optimize_asset_source(
    file_name: &Path,
    bytes: &[u8],
    source_unit_scale_m: f32,
    options: MeshOptimizationOptions,
) -> Result<OptimizedAsset, AssetOptimizationError> {
    let format = source_format(file_name).map_err(|_| AssetOptimizationError::UnsupportedFormat)?;
    if !source_unit_scale_m.is_finite() || source_unit_scale_m <= 0.0 {
        return Err(AssetOptimizationError::InvalidUnitScale);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SOURCE_BYTES {
        return Err(AssetOptimizationError::SourceTooLarge);
    }
    let loaded = load_source::<AssetOptimizationError>(bytes, format, source_unit_scale_m)?;
    Ok(optimize_visual_asset(loaded, options)?)
}

fn glb_limits() -> GlbLoadLimits {
    GlbLoadLimits::try_new(
        4096,
        4096,
        4_000_000,
        12_000_000,
        16_384,
        16 * 1024 * 1024,
        256 * 1024 * 1024,
    )
    .expect("fixed GLB limits are nonzero")
}

/// Failure while preparing configured assets for rendering and physics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetPreparationError {
    /// The source filename does not select STL or GLB ingestion.
    #[error("asset source format is unsupported at {path}")]
    UnsupportedFormat {
        /// Rejected source path.
        path: PathBuf,
    },
    /// Source bytes could not be read.
    #[error("asset source read failed at {path}: {source}")]
    Read {
        /// Source path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Source input exceeded the ingestion bound.
    #[error("asset source at {path} exceeds {maximum} bytes")]
    SourceTooLarge {
        /// Rejected source path.
        path: PathBuf,
        /// Maximum accepted byte count.
        maximum: u64,
    },
    /// STL ingestion failed.
    #[error(transparent)]
    Stl(#[from] StlLoadError),
    /// GLB ingestion failed.
    #[error(transparent)]
    Glb(#[from] GlbLoadError),
    /// Canonical transform application failed.
    #[error(transparent)]
    Canonicalize(#[from] CanonicalizeVisualAssetError),
    /// Convex cache lookup or generation failed.
    #[error(transparent)]
    ConvexCache(#[from] ConvexDecompositionCacheError),
    /// Renderer-neutral aggregate construction failed.
    #[error(transparent)]
    Normalized(#[from] InvalidNormalizedAssetError),
    /// Rapier collision preparation failed.
    #[error(transparent)]
    Physics(#[from] PhysicsAssetPreparationError),
    /// Stable renderer catalog construction failed.
    #[error(transparent)]
    Catalog(#[from] ResolvedAssetCatalogError),
    /// The environment map could not be loaded.
    #[error("environment map {path}: {source}")]
    Environment {
        /// Resolved map path.
        path: PathBuf,
        /// Decoding failure.
        #[source]
        source: bop_assets::EnvironmentLoadError,
    },
}

/// Stores the catalog's visual meshes as the dataset's canonical models.
///
/// Each asset's primitives are merged into one triangle mesh in model-frame
/// metres, which is what exporters need.
pub(crate) fn persist_catalog_models(
    dataset_root: &Path,
    catalog: &ResolvedAssetCatalog,
) -> Result<(), bop_dataset::DatasetOpenError> {
    let models = catalog
        .iter()
        .map(|entry| {
            let mut positions = Vec::new();
            let mut normals = Vec::new();
            let mut indices = Vec::new();
            for primitive in entry.asset().visual_mesh().primitives() {
                let base = u32::try_from(positions.len())
                    .map_err(|_| bop_dataset::InvalidCanonicalModel::TooLarge)?;
                positions.extend_from_slice(primitive.positions());
                normals.extend_from_slice(primitive.normals());
                for index in primitive.indices() {
                    indices.push(
                        index
                            .checked_add(base)
                            .ok_or(bop_dataset::InvalidCanonicalModel::TooLarge)?,
                    );
                }
            }
            bop_dataset::CanonicalModel::try_new(
                entry.asset_id().clone(),
                blake3::Hash::from_bytes(*entry.content_fingerprint())
                    .to_hex()
                    .to_string(),
                positions,
                normals,
                indices,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    bop_dataset::persist_canonical_models(dataset_root, &models)
}
