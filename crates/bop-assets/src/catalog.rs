use std::sync::Arc;

use bop_core::AssetId;
use thiserror::Error;

use crate::{EnvironmentMap, NormalizedAsset};

const CATALOG_FINGERPRINT_DOMAIN: &[u8] = b"bpp.dataset.asset-catalog.v1";
const ENVIRONMENT_FINGERPRINT_DOMAIN: &[u8] = b"bop.dataset.asset-catalog-environment.v1";

/// One checked renderer asset with stable identity and content fingerprint.
pub struct ResolvedAsset {
    asset_id: AssetId,
    content_fingerprint: [u8; 32],
    asset: Arc<NormalizedAsset>,
}

impl ResolvedAsset {
    /// Creates an entry from checked identity, fingerprint, and shared geometry.
    #[must_use]
    pub const fn new(
        asset_id: AssetId,
        content_fingerprint: [u8; 32],
        asset: Arc<NormalizedAsset>,
    ) -> Self {
        Self {
            asset_id,
            content_fingerprint,
            asset,
        }
    }

    /// Returns the stable logical asset identity.
    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    /// Returns the complete content-addressed preprocessing fingerprint.
    #[must_use]
    pub const fn content_fingerprint(&self) -> &[u8; 32] {
        &self.content_fingerprint
    }

    /// Borrows the shared checked renderer-neutral geometry.
    #[must_use]
    pub const fn asset(&self) -> &Arc<NormalizedAsset> {
        &self.asset
    }
}

/// Nonempty identity-ordered renderer asset catalog with a stable fingerprint.
pub struct ResolvedAssetCatalog {
    entries: Vec<ResolvedAsset>,
    fingerprint: String,
    environment: Option<Arc<EnvironmentMap>>,
}

impl ResolvedAssetCatalog {
    /// Sorts unique entries and derives a versioned catalog fingerprint.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty catalog, duplicate asset identity, or
    /// a collection/string length that cannot fit the fixed-width protocol.
    pub fn try_new(mut entries: Vec<ResolvedAsset>) -> Result<Self, ResolvedAssetCatalogError> {
        if entries.is_empty() {
            return Err(ResolvedAssetCatalogError::Empty);
        }
        entries.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        if let Some(duplicate) = entries
            .windows(2)
            .find(|pair| pair[0].asset_id == pair[1].asset_id)
        {
            return Err(ResolvedAssetCatalogError::DuplicateAssetId {
                asset_id: duplicate[0].asset_id.clone(),
            });
        }
        let fingerprint = catalog_fingerprint(&entries)?;
        Ok(Self {
            entries,
            fingerprint,
            environment: None,
        })
    }

    /// Adds the run's environment map. The fingerprint then also covers the
    /// map's content; without one it is unchanged.
    #[must_use]
    pub fn with_environment(mut self, environment: Arc<EnvironmentMap>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ENVIRONMENT_FINGERPRINT_DOMAIN);
        hasher.update(self.fingerprint.as_bytes());
        hasher.update(environment.content_fingerprint());
        self.fingerprint = hasher.finalize().to_hex().to_string();
        self.environment = Some(environment);
        self
    }

    /// Returns the environment map lighting the run, if any.
    #[must_use]
    pub const fn environment(&self) -> Option<&Arc<EnvironmentMap>> {
        self.environment.as_ref()
    }

    /// Returns the number of resolved assets.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the catalog contains no assets.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates entries in ascending [`AssetId`] order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ResolvedAsset> {
        self.entries.iter()
    }

    /// Looks up one entry by stable asset identity.
    #[must_use]
    pub fn get(&self, asset_id: &AssetId) -> Option<&ResolvedAsset> {
        self.entries
            .binary_search_by(|entry| entry.asset_id.cmp(asset_id))
            .ok()
            .map(|index| &self.entries[index])
    }

    /// Returns the versioned lowercase hexadecimal catalog fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Error returned when resolved renderer assets do not form a valid catalog.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ResolvedAssetCatalogError {
    /// A renderer cannot be prepared from an empty asset catalog.
    #[error("resolved asset catalog must not be empty")]
    Empty,
    /// The same stable asset identity appeared more than once.
    #[error("duplicate resolved asset ID {asset_id:?}")]
    DuplicateAssetId {
        /// Duplicated logical identity.
        asset_id: AssetId,
    },
    /// A collection or identifier length exceeded the fixed `u64` framing.
    #[error("resolved asset catalog is too large for the v1 fingerprint protocol")]
    LengthOverflow,
}

fn catalog_fingerprint(entries: &[ResolvedAsset]) -> Result<String, ResolvedAssetCatalogError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CATALOG_FINGERPRINT_DOMAIN);
    hasher.update(
        &u64::try_from(entries.len())
            .map_err(|_| ResolvedAssetCatalogError::LengthOverflow)?
            .to_le_bytes(),
    );
    for entry in entries {
        let id = entry.asset_id.as_str().as_bytes();
        hasher.update(
            &u64::try_from(id.len())
                .map_err(|_| ResolvedAssetCatalogError::LengthOverflow)?
                .to_le_bytes(),
        );
        hasher.update(id);
        hasher.update(&entry.content_fingerprint);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
