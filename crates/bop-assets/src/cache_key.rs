use bop_core::{PositiveMeters, RigidTransform, VhacdParameters};

const PROTOCOL_DOMAIN: &[u8] = b"bpp.dataset.asset-cache-key.v1";

const CURRENT_TOOL_VERSIONS: ToolVersions<'static> = ToolVersions {
    bop_assets: env!("CARGO_PKG_VERSION").as_bytes(),
    stl_io: b"0.11.0",
    gltf: b"1.4.1",
    image: b"0.25.10",
    parry3d: b"0.30.2",
    decomposition_adapter: b"bpp-parry-vhacd-adapter.v1",
};

/// Source container whose geometry enters the asset preprocessing pipeline.
///
/// Variants have fixed protocol labels. Renaming or reordering a variant does
/// not change existing keys.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AssetSourceFormat {
    /// STL input decoded by the pinned `stl_io` adapter.
    Stl,
    /// Binary glTF input decoded by the pinned glTF and image adapters.
    Glb,
}

impl AssetSourceFormat {
    const fn protocol_label(self) -> &'static [u8] {
        match self {
            Self::Stl => b"stl",
            Self::Glb => b"glb",
        }
    }
}

/// Complete checked input to the asset preprocessing cache-key protocol.
///
/// The source path and logical asset ID are intentionally absent: identical
/// bytes processed with identical geometry settings share cached results even
/// when referenced through different paths or catalog entries.
#[derive(Clone, Copy, Debug)]
pub struct AssetCacheKeyInput<'a> {
    source_bytes: &'a [u8],
    source_format: AssetSourceFormat,
    source_unit_scale_m: PositiveMeters,
    canonical_from_model: RigidTransform,
    vhacd: VhacdParameters,
}

impl<'a> AssetCacheKeyInput<'a> {
    /// Creates a complete cache-key input from already checked project types.
    #[must_use]
    pub const fn new(
        source_bytes: &'a [u8],
        source_format: AssetSourceFormat,
        source_unit_scale_m: PositiveMeters,
        canonical_from_model: RigidTransform,
        vhacd: VhacdParameters,
    ) -> Self {
        Self {
            source_bytes,
            source_format,
            source_unit_scale_m,
            canonical_from_model,
            vhacd,
        }
    }
}

/// Stable 256-bit identity for one asset preprocessing result.
///
/// Equality compares the complete BLAKE3 digest. Use [`AssetCacheKey::to_hex`]
/// when a deterministic filesystem-safe representation is needed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetCacheKey([u8; 32]);

impl AssetCacheKey {
    /// Returns the complete binary digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the digest as exactly 64 lowercase hexadecimal characters.
    #[must_use]
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        output
    }
}

/// Derives the stable cache identity for canonical asset preprocessing.
///
/// The v1 protocol hashes exact source bytes, source format, source-unit
/// conversion, canonical transform, every V-HACD parameter, relevant pinned
/// loader versions, the Parry version, and the BOP decomposition-adapter
/// revision. Fields use tagged, length-delimited, little-endian encoding.
#[must_use]
pub fn asset_cache_key(input: AssetCacheKeyInput<'_>) -> AssetCacheKey {
    asset_cache_key_with_versions(input, CURRENT_TOOL_VERSIONS)
}

#[derive(Clone, Copy)]
struct ToolVersions<'a> {
    bop_assets: &'a [u8],
    stl_io: &'a [u8],
    gltf: &'a [u8],
    image: &'a [u8],
    parry3d: &'a [u8],
    decomposition_adapter: &'a [u8],
}

fn asset_cache_key_with_versions(
    input: AssetCacheKeyInput<'_>,
    versions: ToolVersions<'_>,
) -> AssetCacheKey {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROTOCOL_DOMAIN);
    append_field(
        &mut hasher,
        b"source-format",
        input.source_format.protocol_label(),
    );
    append_field(&mut hasher, b"source-bytes", input.source_bytes);
    append_f32(
        &mut hasher,
        b"source-unit-scale-m",
        input.source_unit_scale_m.get(),
    );

    let translation = input.canonical_from_model.translation();
    for (tag, value) in [
        (b"translation-x".as_slice(), translation[0].get()),
        (b"translation-y".as_slice(), translation[1].get()),
        (b"translation-z".as_slice(), translation[2].get()),
    ] {
        append_f32(&mut hasher, tag, value);
    }
    for (tag, value) in [
        (
            b"rotation-x".as_slice(),
            input.canonical_from_model.rotation_xyzw()[0],
        ),
        (
            b"rotation-y".as_slice(),
            input.canonical_from_model.rotation_xyzw()[1],
        ),
        (
            b"rotation-z".as_slice(),
            input.canonical_from_model.rotation_xyzw()[2],
        ),
        (
            b"rotation-w".as_slice(),
            input.canonical_from_model.rotation_xyzw()[3],
        ),
    ] {
        append_f32(&mut hasher, tag, value);
    }

    append_u32(&mut hasher, b"vhacd-resolution", input.vhacd.resolution());
    append_u32(
        &mut hasher,
        b"vhacd-maximum-hulls",
        input.vhacd.max_convex_hulls(),
    );
    append_f32(&mut hasher, b"vhacd-concavity", input.vhacd.concavity());
    append_u32(
        &mut hasher,
        b"vhacd-plane-downsampling",
        input.vhacd.plane_downsampling(),
    );
    append_u32(
        &mut hasher,
        b"vhacd-hull-downsampling",
        input.vhacd.convex_hull_downsampling(),
    );
    append_tool_versions(&mut hasher, input.source_format, versions);

    AssetCacheKey(*hasher.finalize().as_bytes())
}

fn append_tool_versions(
    hasher: &mut blake3::Hasher,
    source_format: AssetSourceFormat,
    versions: ToolVersions<'_>,
) {
    // Frozen v1 label from before the rename; changing it changes every cache key.
    append_field(hasher, b"bpp-assets-version", versions.bop_assets);
    match source_format {
        AssetSourceFormat::Stl => append_field(hasher, b"stl-io-version", versions.stl_io),
        AssetSourceFormat::Glb => {
            append_field(hasher, b"gltf-version", versions.gltf);
            append_field(hasher, b"image-version", versions.image);
        }
    }
    append_field(hasher, b"parry3d-version", versions.parry3d);
    append_field(
        hasher,
        b"decomposition-adapter",
        versions.decomposition_adapter,
    );
}

fn append_u32(hasher: &mut blake3::Hasher, tag: &[u8], value: u32) {
    append_field(hasher, tag, &value.to_le_bytes());
}

fn append_f32(hasher: &mut blake3::Hasher, tag: &[u8], value: f32) {
    append_field(hasher, tag, &value.to_bits().to_le_bytes());
}

fn append_field(hasher: &mut blake3::Hasher, tag: &[u8], value: &[u8]) {
    let tag_length = u64::try_from(tag.len()).expect("field tags fit in u64");
    let value_length = u64::try_from(value.len()).expect("cache-key fields fit in u64");
    hasher
        .update(&tag_length.to_le_bytes())
        .update(tag)
        .update(&value_length.to_le_bytes())
        .update(value);
}

#[cfg(test)]
mod tests {
    use super::{AssetSourceFormat, CURRENT_TOOL_VERSIONS, ToolVersions, append_tool_versions};

    fn tool_digest(format: AssetSourceFormat, versions: ToolVersions<'_>) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        append_tool_versions(&mut hasher, format, versions);
        *hasher.finalize().as_bytes()
    }

    #[test]
    fn every_relevant_tool_version_affects_glb_keys() {
        let baseline = tool_digest(AssetSourceFormat::Glb, CURRENT_TOOL_VERSIONS);
        for versions in [
            ToolVersions {
                bop_assets: b"different",
                ..CURRENT_TOOL_VERSIONS
            },
            ToolVersions {
                gltf: b"different",
                ..CURRENT_TOOL_VERSIONS
            },
            ToolVersions {
                image: b"different",
                ..CURRENT_TOOL_VERSIONS
            },
            ToolVersions {
                parry3d: b"different",
                ..CURRENT_TOOL_VERSIONS
            },
            ToolVersions {
                decomposition_adapter: b"different",
                ..CURRENT_TOOL_VERSIONS
            },
        ] {
            assert_ne!(tool_digest(AssetSourceFormat::Glb, versions), baseline);
        }
    }

    #[test]
    fn stl_loader_version_affects_stl_keys() {
        let baseline = tool_digest(AssetSourceFormat::Stl, CURRENT_TOOL_VERSIONS);
        let changed = ToolVersions {
            stl_io: b"different",
            ..CURRENT_TOOL_VERSIONS
        };
        assert_ne!(tool_digest(AssetSourceFormat::Stl, changed), baseline);
    }
}
