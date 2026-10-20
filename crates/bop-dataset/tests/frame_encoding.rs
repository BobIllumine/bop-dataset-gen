//! Canonical renderer-frame validation and encoding contracts.

use std::{fs::File, io::BufReader};

use bop_dataset::{DepthFrame, FrameValidationError, InstanceFrame, RgbFrame};
use exr::prelude::*;

#[test]
fn frame_dimensions_and_buffer_lengths_are_validated_before_encoding() {
    assert_eq!(
        RgbFrame::try_new(0, 2, vec![]).unwrap_err(),
        FrameValidationError::ZeroDimensions
    );
    assert_eq!(
        RgbFrame::try_new(2, 2, vec![0; 11]).unwrap_err(),
        FrameValidationError::InvalidDataLength {
            expected: 12,
            actual: 11,
        }
    );
    assert_eq!(
        DepthFrame::try_new(2, 2, vec![0.0; 3]).unwrap_err(),
        FrameValidationError::InvalidDataLength {
            expected: 4,
            actual: 3,
        }
    );
    assert_eq!(
        InstanceFrame::try_new(2, 2, vec![0; 5]).unwrap_err(),
        FrameValidationError::InvalidDataLength {
            expected: 4,
            actual: 5,
        }
    );
}

#[test]
fn depth_requires_finite_nonnegative_metric_values() {
    assert_eq!(
        DepthFrame::try_new(2, 1, vec![1.0, f32::NAN]).unwrap_err(),
        FrameValidationError::NonFiniteDepth { index: 1 }
    );
    assert_eq!(
        DepthFrame::try_new(2, 1, vec![1.0, -0.25]).unwrap_err(),
        FrameValidationError::NegativeDepth { index: 1 }
    );
    assert!(DepthFrame::try_new(2, 1, vec![0.0, 12.5]).is_ok());
}

#[test]
fn rgb_is_written_as_exact_eight_bit_srgb_png() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rgb.png");
    let pixels = vec![255, 0, 1, 2, 127, 254, 8, 9, 10, 11, 12, 13];
    RgbFrame::try_new(2, 2, pixels.clone())
        .unwrap()
        .write_png(&path, bop_core::PngCompression::Fast)
        .unwrap();

    let decoder = png::Decoder::new(BufReader::new(File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    assert_eq!(reader.info().width, 2);
    assert_eq!(reader.info().height, 2);
    assert_eq!(reader.info().color_type, png::ColorType::Rgb);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
    assert!(reader.info().srgb.is_some());
    let mut output_bytes = vec![0; reader.output_buffer_size().unwrap()];
    let output = reader.next_frame(&mut output_bytes).unwrap();
    assert_eq!(&output_bytes[..output.buffer_size()], pixels);
}

#[test]
fn instance_ids_are_written_losslessly_as_sixteen_bit_grayscale_png() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("instance.png");
    let ids = vec![0, 1, 256, u16::MAX];
    InstanceFrame::try_new(2, 2, ids.clone())
        .unwrap()
        .write_png(&path, bop_core::PngCompression::Fast)
        .unwrap();

    let decoder = png::Decoder::new(BufReader::new(File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    assert_eq!(reader.info().width, 2);
    assert_eq!(reader.info().height, 2);
    assert_eq!(reader.info().color_type, png::ColorType::Grayscale);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
    let mut output_bytes = vec![0; reader.output_buffer_size().unwrap()];
    let output = reader.next_frame(&mut output_bytes).unwrap();
    let decoded_ids: Vec<u16> = output_bytes[..output.buffer_size()]
        .chunks_exact(2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(decoded_ids, ids);
}

#[test]
fn depth_is_written_losslessly_as_single_channel_float_exr() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("depth.exr");
    let depths = vec![0.0, 0.125, 12.5, 1000.25];
    DepthFrame::try_new(2, 2, depths.clone())
        .unwrap()
        .write_exr(&path)
        .unwrap();

    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("Z")
        .collect_pixels(
            |resolution, _| vec![0.0_f32; resolution.area()],
            |samples, position, (depth,): (f32,)| {
                samples[position.y() * 2 + position.x()] = depth;
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap();
    assert_eq!(image.layer_data.size, Vec2(2, 2));
    assert_eq!(image.layer_data.channel_data.pixels, depths);
}

#[test]
fn every_png_compression_level_stores_the_same_pixels() {
    let directory = tempfile::tempdir().unwrap();
    // A smooth gradient with noise, like a shaded render.
    let pixels: Vec<u8> = (0..64 * 48 * 3)
        .map(|i| u8::try_from((i / 3 % 64 + i / (64 * 3) + i * 7 % 5) % 256).unwrap())
        .collect();
    let frame = RgbFrame::try_new(64, 48, pixels.clone()).unwrap();
    for level in [
        bop_core::PngCompression::Fast,
        bop_core::PngCompression::Balanced,
        bop_core::PngCompression::High,
    ] {
        let path = directory.path().join(format!("{level:?}.png"));
        frame.write_png(&path, level).unwrap();
        let decoder = png::Decoder::new(BufReader::new(File::open(path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut output).unwrap();
        assert_eq!(&output[..info.buffer_size()], pixels, "{level:?}");
    }
}
