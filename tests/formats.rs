use image::{ImageEncoder, ImageFormat, Rgba, RgbaImage, codecs::png::PngEncoder};
use light_image_tool::{Cancellation, ToolError, icon_container::IconEncoding, pipeline};
use std::{fs, path::Path};

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let pixels = RgbaImage::from_pixel(width, height, Rgba(color));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes
}

fn dib(width: u32, height: u32, depth: u16, color: [u8; 4], mask_first: bool) -> Vec<u8> {
    let row_bytes = (width * u32::from(depth)).div_ceil(32) * 4;
    let mask_row_bytes = width.div_ceil(32) * 4;
    let mut bytes = vec![0; 40];
    bytes[..4].copy_from_slice(&40u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&width.to_le_bytes());
    bytes[8..12].copy_from_slice(&(height * 2).to_le_bytes());
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&depth.to_le_bytes());
    for _ in 0..height {
        let row_start = bytes.len();
        for _ in 0..width {
            bytes.extend_from_slice(&[color[2], color[1], color[0]]);
            if depth == 32 {
                bytes.push(color[3]);
            }
        }
        bytes.resize(row_start + row_bytes as usize, 0);
    }
    for y in 0..height {
        let row_start = bytes.len();
        bytes.resize(row_start + mask_row_bytes as usize, 0);
        if mask_first && y == height - 1 {
            bytes[row_start] = 0x80;
        }
    }
    bytes
}

fn ico(entries: &[(u32, u32, u16, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 1, 0];
    bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + entries.len() * 16;
    for (width, height, depth, payload) in entries {
        bytes.extend_from_slice(&[(*width % 256) as u8, (*height % 256) as u8, 0, 0]);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&depth.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += payload.len();
    }
    for (_, _, _, payload) in entries {
        bytes.extend_from_slice(payload);
    }
    bytes
}

fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
}

#[test]
fn bmp_is_detected_by_content_and_preserves_bottom_up_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disguised.png");
    let image = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 90, y as u8 * 120, 17, 255]));
    image.save_with_format(&path, ImageFormat::Bmp).unwrap();
    let original = fs::read(&path).unwrap();
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert_eq!(loaded.format, ImageFormat::Bmp);
    assert_eq!(*loaded.pixels, image);
    assert!(loaded.icon.is_none());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        pipeline::thumbnail(&path, 2, &Cancellation::default())
            .unwrap()
            .dimensions(),
        (2, 1)
    );
}

#[test]
fn ico_default_prefers_largest_area_then_depth_and_explicit_size_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sizes.jpg");
    let bytes = ico(&[
        (16, 16, 32, png(16, 16, [255, 0, 0, 128])),
        (32, 32, 24, dib(32, 32, 24, [0, 255, 0, 255], false)),
        (32, 32, 32, png(32, 32, [0, 0, 255, 64])),
    ]);
    write(&path, &bytes);
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert_eq!(loaded.format, ImageFormat::Ico);
    assert_eq!(loaded.pixels.dimensions(), (32, 32));
    assert_eq!(loaded.pixels.get_pixel(0, 0).0, [0, 0, 255, 64]);
    assert!(loaded.has_alpha);
    let directory = loaded.icon.as_ref().unwrap();
    assert_eq!(directory.selected_index, 2);
    assert_eq!(directory.entries.len(), 3);
    assert_eq!(directory.entries[1].encoding, IconEncoding::Dib);
    assert_eq!(directory.entries[1].bit_depth, 24);
    let smaller = pipeline::load_icon_entry(&path, 0, &Cancellation::default()).unwrap();
    assert_eq!(smaller.pixels.dimensions(), (16, 16));
    assert_eq!(smaller.pixels.get_pixel(0, 0).0, [255, 0, 0, 128]);
    assert_eq!(smaller.icon.as_ref().unwrap().selected_index, 0);
    assert_eq!(loaded.source_sha256, smaller.source_sha256);
    let thumbnail = pipeline::thumbnail(&path, 8, &Cancellation::default()).unwrap();
    assert_eq!(thumbnail.dimensions(), (8, 8));
    assert_eq!(thumbnail.get_pixel(0, 0).0, [0, 0, 255, 64]);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn ico_reload_matches_dimensions_and_depth_after_directory_reordering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reordered.ico");
    let small = (16, 16, 32, png(16, 16, [255, 0, 0, 255]));
    let matching = (32, 32, 24, dib(32, 32, 24, [0, 255, 0, 255], false));
    let deeper = (32, 32, 32, png(32, 32, [0, 0, 255, 255]));
    write(
        &path,
        &ico(&[small.clone(), matching.clone(), deeper.clone()]),
    );
    let original = pipeline::load_icon_entry(&path, 1, &Cancellation::default()).unwrap();
    assert_eq!(original.icon.as_ref().unwrap().selected_index, 1);
    write(&path, &ico(&[deeper.clone(), small.clone(), matching]));
    let reordered = pipeline::reload_icon(&path, [32, 32], 24, &Cancellation::default()).unwrap();
    assert_eq!(reordered.icon.as_ref().unwrap().selected_index, 2);
    assert_eq!(reordered.pixels.get_pixel(0, 0).0, [0, 255, 0, 255]);
    assert_ne!(reordered.source_sha256, original.source_sha256);
    // If the exact depth disappears, prefer the highest available depth at that size.
    write(&path, &ico(&[small, deeper]));
    let deeper = pipeline::reload_icon(&path, [32, 32], 24, &Cancellation::default()).unwrap();
    assert_eq!(deeper.icon.as_ref().unwrap().selected_index, 1);
    assert_eq!(deeper.pixels.get_pixel(0, 0).0, [0, 0, 255, 255]);
}

#[test]
fn ico_reload_uses_current_default_when_size_is_removed_and_detects_replacement_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replaced.ico");
    write(
        &path,
        &ico(&[
            (16, 16, 32, png(16, 16, [255, 0, 0, 255])),
            (64, 64, 32, png(64, 64, [0, 255, 0, 255])),
        ]),
    );
    let current = pipeline::reload_icon(&path, [32, 32], 32, &Cancellation::default()).unwrap();
    assert_eq!(current.pixels.dimensions(), (64, 64));
    assert_eq!(current.icon.as_ref().unwrap().selected_index, 1);
    write(&path, &png(3, 2, [70, 80, 90, 128]));
    let replaced = pipeline::reload_icon(&path, [32, 32], 32, &Cancellation::default()).unwrap();
    assert_eq!(replaced.format, ImageFormat::Png);
    assert!(replaced.icon.is_none());
    assert_eq!(replaced.pixels.dimensions(), (3, 2));
    assert_eq!(replaced.pixels.get_pixel(0, 0).0, [70, 80, 90, 128]);
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(matches!(
        pipeline::reload_icon(&path, [32, 32], 32, &cancelled),
        Err(ToolError::Cancelled)
    ));
}

#[test]
fn ico_dib_supports_legacy_and_mask_and_per_pixel_alpha() {
    let dir = tempfile::tempdir().unwrap();
    for depth in [24, 32] {
        let path = dir.path().join(format!("mask-{depth}.ico"));
        write(
            &path,
            &ico(&[(2, 2, depth, dib(2, 2, depth, [71, 112, 93, 128], true))]),
        );
        let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
        assert_eq!(loaded.pixels.get_pixel(0, 0).0, [71, 112, 93, 0]);
        assert_eq!(
            loaded.pixels.get_pixel(1, 0).0,
            [71, 112, 93, if depth == 32 { 128 } else { 255 }]
        );
        assert!(loaded.has_alpha);
        assert_eq!(
            *loaded.pixels,
            pipeline::thumbnail(&path, 2, &Cancellation::default()).unwrap()
        );
    }
}

#[test]
fn ico_legacy_zero_alpha_restores_opacity_before_applying_and_mask() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.ico");
    let source = ico(&[(2, 2, 32, dib(2, 2, 32, [30, 60, 90, 0], true))]);
    write(&path, &source);
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert_eq!(loaded.pixels.get_pixel(0, 0).0, [30, 60, 90, 0]);
    assert_eq!(loaded.pixels.get_pixel(1, 0).0, [30, 60, 90, 255]);
    assert_eq!(loaded.pixels.get_pixel(0, 1).0, [30, 60, 90, 255]);
    assert!(loaded.has_alpha);
    assert_eq!(
        *loaded.pixels,
        pipeline::thumbnail(&path, 2, &Cancellation::default()).unwrap()
    );
    assert_eq!(fs::read(&path).unwrap(), source);
}

#[test]
fn ico_modern_transparent_png_and_explicit_dib_alpha_mask_stay_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transparent.ico");
    let old_header = dib(2, 2, 32, [30, 60, 90, 0], false);
    let mut explicit = vec![0; 56];
    explicit[..40].copy_from_slice(&old_header[..40]);
    explicit[..4].copy_from_slice(&56u32.to_le_bytes());
    explicit[52..56].copy_from_slice(&0xff00_0000u32.to_le_bytes());
    explicit.extend_from_slice(&old_header[40..]);
    write(
        &path,
        &ico(&[(2, 2, 32, png(2, 2, [30, 60, 90, 0])), (2, 2, 32, explicit)]),
    );
    for index in [0, 1] {
        let loaded = pipeline::load_icon_entry(&path, index, &Cancellation::default()).unwrap();
        assert!(loaded.pixels.pixels().all(|pixel| pixel[3] == 0));
        assert!(loaded.has_alpha);
    }
}

#[test]
fn ico_zero_size_means_256_and_zero_declared_depth_uses_payload_depth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("full.ico");
    write(&path, &ico(&[(256, 256, 0, png(256, 256, [1, 2, 3, 255]))]));
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert_eq!(loaded.pixels.dimensions(), (256, 256));
    assert_eq!(loaded.icon.unwrap().entries[0].bit_depth, 32);
}

#[test]
fn ico_rejects_directory_boundary_dimension_and_depth_violations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.ico");
    let valid = ico(&[(2, 2, 32, png(2, 2, [1, 2, 3, 255]))]);
    let mut cases = Vec::new();
    let mut offset_in_directory = valid.clone();
    offset_in_directory[18..22].copy_from_slice(&6u32.to_le_bytes());
    cases.push(offset_in_directory);
    let mut overflowing_range = valid.clone();
    overflowing_range[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(overflowing_range);
    let mut truncated = valid.clone();
    truncated.pop();
    cases.push(truncated);
    let mut dimensions = valid.clone();
    dimensions[6] = 3;
    cases.push(dimensions);
    let mut depth = ico(&[(2, 2, 32, dib(2, 2, 32, [1, 2, 3, 255], false))]);
    depth[12..14].copy_from_slice(&24u16.to_le_bytes());
    cases.push(depth);
    cases.push(vec![0, 0, 1, 0, 0, 0]);
    cases.push(vec![0, 0, 1, 0, 1, 0]);
    for bytes in cases {
        write(&path, &bytes);
        assert!(pipeline::load(&path, &Cancellation::default()).is_err());
        assert!(pipeline::thumbnail(&path, 8, &Cancellation::default()).is_err());
    }
}

#[test]
fn ico_png_rgb_and_indexed_transparency_are_supported_despite_directory_hint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("png-types.ico");
    let mut rgb = Vec::new();
    PngEncoder::new(&mut rgb)
        .write_image(&[10, 20, 30], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    let mut indexed = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut indexed, 1, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(vec![40, 50, 60]);
        encoder.set_trns(vec![96]);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0])
            .unwrap();
    }
    write(&path, &ico(&[(1, 1, 32, rgb), (1, 1, 32, indexed)]));
    let rgb = pipeline::load_icon_entry(&path, 0, &Cancellation::default()).unwrap();
    assert_eq!(rgb.pixels.get_pixel(0, 0).0, [10, 20, 30, 255]);
    assert_eq!(rgb.icon.unwrap().entries[0].bit_depth, 24);
    let indexed = pipeline::load_icon_entry(&path, 1, &Cancellation::default()).unwrap();
    assert_eq!(indexed.pixels.get_pixel(0, 0).0, [40, 50, 60, 96]);
    assert!(indexed.has_alpha);
}

#[test]
fn ico_selection_rejects_other_formats_and_out_of_range_and_honors_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("single.ico");
    write(&path, &ico(&[(2, 2, 32, png(2, 2, [1, 2, 3, 255]))]));
    assert!(pipeline::load_icon_entry(&path, 1, &Cancellation::default()).is_err());
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(matches!(
        pipeline::load_icon_entry(&path, 0, &cancelled),
        Err(ToolError::Cancelled)
    ));
    write(&path, &png(2, 2, [1, 2, 3, 255]));
    assert!(pipeline::load_icon_entry(&path, 0, &Cancellation::default()).is_err());
}

#[test]
fn malformed_largest_ico_payload_is_reported_without_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken-large.ico");
    let mut broken = png(32, 32, [1, 2, 3, 255]);
    broken.truncate(33);
    write(
        &path,
        &ico(&[(2, 2, 32, png(2, 2, [4, 5, 6, 255])), (32, 32, 32, broken)]),
    );
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
    assert!(pipeline::load_icon_entry(&path, 0, &Cancellation::default()).is_ok());
}

#[test]
fn huge_bmp_dimensions_are_rejected_before_pixel_decode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.bmp");
    let mut bytes = vec![0; 54];
    bytes[..2].copy_from_slice(b"BM");
    bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&4001u32.to_le_bytes());
    bytes[22..26].copy_from_slice(&4000u32.to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    write(&path, &bytes);
    let error = pipeline::load(&path, &Cancellation::default())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("1600"), "{error}");
    assert!(pipeline::thumbnail(&path, 8, &Cancellation::default()).is_err());
}

#[test]
fn bmp_v5_embedded_profile_is_color_managed_and_linked_profile_is_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.bmp");
    let profile = moxcms::ColorProfile::new_srgb().encode().unwrap();
    let mut bytes = vec![0; 14 + 124];
    bytes[..2].copy_from_slice(b"BM");
    bytes[10..14].copy_from_slice(&138u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&124u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&1u32.to_le_bytes());
    bytes[22..26].copy_from_slice(&1u32.to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    bytes[70..74].copy_from_slice(&0x4d42_4544u32.to_le_bytes());
    bytes[126..130].copy_from_slice(&128u32.to_le_bytes());
    bytes[130..134].copy_from_slice(&(profile.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[30, 20, 10, 0]);
    bytes.extend_from_slice(&profile);
    write(&path, &bytes);
    let embedded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(embedded.color_managed);
    assert!(embedded.color_status.contains("ICC 已转换"));
    bytes[70..74].copy_from_slice(&0x4c49_4e4bu32.to_le_bytes());
    write(&path, &bytes);
    let linked = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(!linked.color_managed);
    assert!(
        linked
            .warnings
            .iter()
            .any(|warning| warning.contains("未读取外部文件"))
    );
    assert_eq!(linked.pixels.get_pixel(0, 0).0, [10, 20, 30, 255]);
    bytes[70..74].copy_from_slice(&0x4d42_4544u32.to_le_bytes());
    bytes[126..130].copy_from_slice(&u32::MAX.to_le_bytes());
    write(&path, &bytes);
    let malformed = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(!malformed.color_managed);
    assert!(
        malformed
            .warnings
            .iter()
            .any(|warning| warning.contains("已截断"))
    );
}
