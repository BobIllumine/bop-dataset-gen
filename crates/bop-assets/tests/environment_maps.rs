//! Environment maps: decoding, validation, and the catalog fingerprint.

use std::{path::Path, sync::Arc};

use bop_assets::{EnvironmentLoadError, load_environment};

/// A flat (uncompressed) Radiance file of `width`x`height` texels of `rgb`.
fn radiance(width: u32, height: u32, rgb: [f32; 3]) -> Vec<u8> {
    let max = rgb.iter().copied().fold(0.0_f32, f32::max);
    let (_, exponent) = frexp(max);
    let scale = 256.0 / 2.0_f32.powi(exponent);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "values below 256"
    )]
    let texel = [
        (rgb[0] * scale) as u8,
        (rgb[1] * scale) as u8,
        (rgb[2] * scale) as u8,
        u8::try_from(exponent + 128).unwrap(),
    ];
    let mut bytes =
        format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
    for _ in 0..width * height {
        bytes.extend(texel);
    }
    bytes
}

#[allow(clippy::cast_possible_truncation, reason = "test radiances are small")]
fn frexp(value: f32) -> (f32, i32) {
    let exponent = value.log2().floor() as i32 + 1;
    (value / 2.0_f32.powi(exponent), exponent)
}

#[test]
fn radiance_hdr_decodes_to_linear_rgb() {
    let map = load_environment(Path::new("sky.hdr"), &radiance(8, 4, [0.5, 1.0, 2.0])).unwrap();
    assert_eq!(map.dimensions(), [8, 4]);
    assert_eq!(map.rgb().len(), 8 * 4 * 3);
    for texel in map.rgb().chunks_exact(3) {
        assert!(
            (texel[0] - 0.5).abs() < 0.01
                && (texel[1] - 1.0).abs() < 0.01
                && (texel[2] - 2.0).abs() < 0.02,
            "{texel:?}"
        );
    }
    assert_eq!(
        map.content_fingerprint(),
        blake3::hash(&radiance(8, 4, [0.5, 1.0, 2.0])).as_bytes()
    );
}

#[test]
fn openexr_decodes_rgb_channels() {
    use exr::prelude::{Image, SpecificChannels, Vec2, WritableImage};
    let pixels = SpecificChannels::rgb(|position: Vec2<usize>| {
        (
            f32::from(u8::try_from(position.x()).unwrap()),
            0.25_f32,
            3.0_f32,
        )
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    Image::from_channels((8, 4), pixels)
        .write()
        .to_buffered(&mut bytes)
        .unwrap();
    let map = load_environment(Path::new("sky.exr"), bytes.get_ref()).unwrap();
    assert_eq!(map.dimensions(), [8, 4]);
    assert_eq!(&map.rgb()[..6], &[0.0, 0.25, 3.0, 1.0, 0.25, 3.0]);
}

#[test]
fn unusable_maps_are_rejected() {
    assert!(matches!(
        load_environment(Path::new("sky.png"), &[]),
        Err(EnvironmentLoadError::UnsupportedFormat)
    ));
    assert!(matches!(
        load_environment(Path::new("square.hdr"), &radiance(4, 4, [1.0; 3])),
        Err(EnvironmentLoadError::Dimensions {
            width: 4,
            height: 4
        })
    ));
    assert!(matches!(
        load_environment(Path::new("broken.hdr"), b"not radiance"),
        Err(EnvironmentLoadError::Hdr(_))
    ));
}

#[test]
fn the_catalog_fingerprint_covers_the_environment_only_when_present() {
    let catalog = || {
        let primitive = bop_assets::VisualPrimitive::try_new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0.0, 0.0, 1.0]; 3],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap();
        let part = bop_assets::ConvexPart::try_new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
        )
        .unwrap();
        let asset = bop_assets::NormalizedAsset::try_new(
            bop_assets::VisualMesh::try_new(vec![primitive]).unwrap(),
            vec![],
            vec![],
            vec![part],
        )
        .unwrap();
        bop_assets::ResolvedAssetCatalog::try_new(vec![bop_assets::ResolvedAsset::new(
            bop_core::AssetId::new("a").unwrap(),
            [7; 32],
            Arc::new(asset),
        )])
        .unwrap()
    };
    let plain = catalog();
    let fingerprint = plain.fingerprint().to_owned();
    assert!(plain.environment().is_none());
    let sky = Arc::new(load_environment(Path::new("sky.hdr"), &radiance(8, 4, [1.0; 3])).unwrap());
    let lit = catalog().with_environment(sky);
    assert_ne!(lit.fingerprint(), fingerprint);
    let dim = Arc::new(load_environment(Path::new("sky.hdr"), &radiance(8, 4, [0.5; 3])).unwrap());
    assert_ne!(
        catalog().with_environment(dim).fingerprint(),
        lit.fingerprint()
    );
}
