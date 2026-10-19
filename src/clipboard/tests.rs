use super::{codec::*, *};
use image::ImageEncoder;

fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn dib24(width: u32, height: i32) -> Vec<u8> {
    let stride = (width as usize * 3).next_multiple_of(4);
    let mut bytes = vec![0; 40 + stride * height.unsigned_abs() as usize];
    put(&mut bytes, 0, 40);
    put(&mut bytes, 4, width);
    put(&mut bytes, 8, height as u32);
    put(&mut bytes, 12, 1 | (24 << 16));
    bytes
}
fn png_rgba() -> RgbaImage {
    RgbaImage::from_raw(
        2,
        2,
        vec![
            255, 0, 0, 0, 10, 20, 30, 1, 40, 50, 60, 128, 70, 80, 90, 255,
        ],
    )
    .unwrap()
}

#[test]
fn dimensions_are_checked_without_allocating_pixels() {
    assert_eq!(rgba_len(4000, 4000).unwrap(), 64_000_000);
    assert!(rgba_len(4001, 4000).is_err());
    assert!(rgba_len(32768, 1).is_ok());
    assert!(rgba_len(32769, 1).is_err());
    assert!(rgba_len(u32::MAX, u32::MAX).is_err());
    assert!(rgba_len(0, 2).is_err());
}

#[test]
fn png_ihdr_is_rejected_before_decode_or_pixel_allocation() {
    let mut bytes = encode_png(&png_rgba()).unwrap();
    assert_eq!(png_dimensions(&bytes).unwrap(), (2, 2));
    bytes[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        png_dimensions(&bytes),
        Err(ClipboardError::TooLarge(_))
    ));
    for length in 0..33 {
        assert!(png_dimensions(&bytes[..length]).is_err());
    }
    bytes[0] = 0;
    assert!(png_dimensions(&bytes).is_err());
}

#[test]
fn png_round_trip_retains_every_alpha_and_hidden_rgb_byte() {
    let expected = png_rgba();
    let result = decode_png(&encode_png(&expected).unwrap()).unwrap();
    assert_eq!(&**result.pixels, &expected);
    assert_eq!(result.source, ClipboardSource::Png);
    assert!(result.color_status.contains("未知"));
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("没有可核验的原文件"))
    );
}

#[test]
fn png_truncated_or_bad_crc_is_rejected() {
    let encoded = encode_png(&png_rgba()).unwrap();
    for length in 33..encoded.len() {
        assert!(decode_png(&encoded[..length]).is_err(), "length {length}");
    }
    let mut damaged = encoded;
    damaged[29] ^= 1;
    assert!(decode_png(&damaged).is_err());
}

#[test]
fn clipboard_png_preserves_icc_alpha_and_reuses_color_conversion() {
    let pixels = png_rgba();
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
        .unwrap();
    encoder
        .write_image(pixels.as_raw(), 2, 2, image::ExtendedColorType::Rgba8)
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert!(result.color_managed);
    assert!(result.color_status.contains("ICC 已转换"));
    assert_eq!(
        result.pixels.pixels().map(|p| p[3]).collect::<Vec<_>>(),
        [0, 1, 128, 255]
    );
}

#[test]
fn clipboard_png_bad_icc_has_explicit_unmanaged_status() {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder.set_icc_profile(vec![1, 2, 3, 4]).unwrap();
    encoder
        .write_image(&[1, 2, 3, 4], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert!(!result.color_managed);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("ICC 无效"))
    );
}

#[test]
fn clipboard_png_large_icc_is_checked_borrowed_before_profile_parse() {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(vec![0; crate::pipeline::MAX_ICC_BYTES + 1])
        .unwrap();
    encoder
        .write_image(&[1, 2, 3, 4], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert!(!result.color_managed);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("4 MiB"))
    );
}

#[test]
fn clipboard_png_gray_icc_and_sixteen_bit_are_supported_with_warning() {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(
            moxcms::ColorProfile::new_gray_with_gamma(1.0)
                .encode()
                .unwrap(),
        )
        .unwrap();
    encoder
        .write_image(&[128, 64], 1, 1, image::ExtendedColorType::La8)
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert!(result.color_managed);
    assert_eq!(result.pixels.get_pixel(0, 0)[3], 64);
    assert!(result.pixels.get_pixel(0, 0)[0] > 180);

    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Sixteen);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[0x12, 0x34, 0x56, 0x78, 0x90, 0xab, 0xcd, 0xef])
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert_eq!(result.pixels.get_pixel(0, 0).0, [0x12, 0x56, 0x90, 0xcd]);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("8 位"))
    );
}

#[test]
fn clipboard_png_apng_is_explicitly_unsupported() {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(1, 0).unwrap();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[1, 2, 3, 4])
        .unwrap();
    assert!(matches!(
        decode_png(&bytes),
        Err(ClipboardError::UnsupportedData(_))
    ));
}

#[test]
fn dibv5_round_trip_preserves_transparency_and_bottom_up_rows() {
    let expected = png_rgba();
    let encoded = encode_dibv5(&expected).unwrap();
    let layout = dib_layout(&encoded, ClipboardSource::DibV5).unwrap();
    assert!(!layout.top_down && layout.alpha && layout.srgb);
    let result = decode_dib(&encoded, ClipboardSource::DibV5).unwrap();
    assert_eq!(&**result.pixels, &expected);
}

#[test]
fn chromium_style_bi_rgb_dibv5_preserves_explicit_alpha_only() {
    for zero_rgb_masks in [false, true] {
        let expected = png_rgba();
        let mut encoded = encode_dibv5(&expected).unwrap();
        put(&mut encoded, 16, 0);
        if zero_rgb_masks {
            put(&mut encoded, 40, 0);
            put(&mut encoded, 44, 0);
            put(&mut encoded, 48, 0);
        }
        assert_eq!(
            &**decode_dib(&encoded, ClipboardSource::DibV5).unwrap().pixels,
            &expected
        );
        put(&mut encoded, 52, 0x0f000000);
        assert!(dib_layout(&encoded, ClipboardSource::DibV5).is_err());
    }
}

#[test]
fn dib24_padding_and_top_down_are_not_interpreted_as_pixels() {
    let mut bytes = dib24(1, -2);
    bytes[40..48].copy_from_slice(&[3, 2, 1, 200, 6, 5, 4, 201]);
    let result = decode_dib(&bytes, ClipboardSource::Dib).unwrap();
    assert_eq!(result.pixels.as_raw(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    put(&mut bytes, 8, 2);
    let result = decode_dib(&bytes, ClipboardSource::Dib).unwrap();
    assert_eq!(result.pixels.as_raw(), &[4, 5, 6, 255, 1, 2, 3, 255]);
}

#[test]
fn dib32_bi_rgb_reserved_byte_is_opaque_without_explicit_v5_alpha() {
    let mut bytes = dib24(1, 1);
    put(&mut bytes, 12, 1 | (32 << 16));
    bytes[40..44].copy_from_slice(&[30, 20, 10, 0]);
    assert_eq!(
        decode_dib(&bytes, ClipboardSource::Dib)
            .unwrap()
            .pixels
            .get_pixel(0, 0)
            .0,
        [10, 20, 30, 255]
    );
}

#[test]
fn dib_accepts_only_narrow_headers_depths_masks_and_exact_bounds() {
    let valid = encode_dibv5(&png_rgba()).unwrap();
    for (offset, value) in [
        (0, 108),
        (4, u32::MAX),
        (8, i32::MIN as u32),
        (12, 32 << 16),
        (12, 1 | (16 << 16)),
        (16, 1),
        (32, 1),
        (40, 0x0f000000),
        (44, 0xff00ff00),
        (52, 0x0f000000),
        (56, 0x4c494e4b),
        (112, 124),
        (116, 2),
        (120, 1),
        (20, 1),
        (20, u32::MAX),
    ] {
        let mut bytes = valid.clone();
        put(&mut bytes, offset, value);
        assert!(
            dib_layout(&bytes, ClipboardSource::DibV5).is_err(),
            "field {offset}={value}"
        );
    }
    for length in 0..valid.len() {
        assert!(dib_layout(&valid[..length], ClipboardSource::DibV5).is_err());
    }
}

#[test]
fn dib40_bitfields_standard_masks_have_no_invented_alpha() {
    let mut bytes = dib24(1, 1);
    bytes.resize(56, 0);
    put(&mut bytes, 12, 1 | (32 << 16));
    put(&mut bytes, 16, 3);
    put(&mut bytes, 40, 0x00ff0000);
    put(&mut bytes, 44, 0x0000ff00);
    put(&mut bytes, 48, 0x000000ff);
    bytes[52..].copy_from_slice(&[3, 2, 1, 0]);
    assert_eq!(
        decode_dib(&bytes, ClipboardSource::Dib)
            .unwrap()
            .pixels
            .get_pixel(0, 0)
            .0,
        [1, 2, 3, 255]
    );
}

#[test]
fn path_requires_one_absolute_local_image_and_never_a_url_device_or_unc() {
    assert_eq!(
        local_path("  \"C:\\图片\\透明.PNG\"  ").unwrap(),
        PathBuf::from("C:\\图片\\透明.PNG")
    );
    assert_eq!(
        local_path("D:/folder/a.webp").unwrap(),
        PathBuf::from("D:\\folder\\a.webp")
    );
    for bad in [
        "https://site/a.png",
        "file:///C:/a.png",
        "\\\\host\\share\\a.png",
        "//host/share/a.png",
        "\\\\?\\C:\\a.png",
        "\\\\.\\C:\\a.png",
        "C:a.png",
        "\\a.png",
        "a.png",
        "C:\\a.png\nD:\\b.png",
        "C:\\a.png:stream",
        "C:\\a?b.png",
        "C:\\..\\a.png",
        "C:\\a.\\b.png",
        "C:\\NUL.png",
        "C:\\COM1.png",
        "C:\\COM¹.png",
        "C:\\LPT².png",
        "C:\\a.txt",
        "C:\\\\a.png",
    ] {
        assert!(local_path(bad).is_err(), "{bad}");
    }
    assert!(matches!(
        local_path(&format!("C:\\{}.png", "a".repeat(MAX_PATH_UNITS))),
        Err(ClipboardError::TooLarge(_))
    ));
}

#[test]
fn unicode_text_requires_terminator_valid_pairs_and_bounded_size() {
    let expected = "C:\\图片\\🌙.png";
    assert_eq!(
        decode_unicode_text(&encode_text(expected).unwrap()).unwrap(),
        expected
    );
    assert!(decode_unicode_text(&[b'A', 0]).is_err());
    assert!(decode_unicode_text(&[0, 0xd8, 0, 0]).is_err());
    assert!(decode_unicode_text(&[1]).is_err());
    assert!(encode_text("a\0b").is_err());
    assert!(matches!(
        encode_text(&"a".repeat(MAX_PATH_UNITS + 1)),
        Err(ClipboardError::TooLarge(_))
    ));
}

#[test]
fn hdrop_requires_one_unicode_path_and_valid_double_terminator() {
    let text = encode_text("C:\\图像.png").unwrap();
    let mut bytes = vec![0; 20];
    put(&mut bytes, 0, 20);
    put(&mut bytes, 16, 1);
    bytes.extend_from_slice(&text);
    bytes.extend_from_slice(&[0, 0]);
    assert_eq!(decode_hdrop(&bytes).unwrap(), PathBuf::from("C:\\图像.png"));
    let mut multiple = bytes[..bytes.len() - 2].to_vec();
    multiple.extend_from_slice(&encode_text("D:\\two.png").unwrap());
    multiple.extend_from_slice(&[0, 0]);
    assert!(matches!(
        decode_hdrop(&multiple),
        Err(ClipboardError::MultipleFiles)
    ));
    let mut ansi = bytes.clone();
    put(&mut ansi, 16, 0);
    assert!(matches!(
        decode_hdrop(&ansi),
        Err(ClipboardError::UnsupportedData(_))
    ));
    let mut offset = bytes.clone();
    put(&mut offset, 0, u32::MAX);
    assert!(decode_hdrop(&offset).is_err());
    assert!(decode_hdrop(&bytes[..bytes.len() - 2]).is_err());
}

#[test]
fn malformed_small_inputs_never_panic() {
    // Deterministic adversarial byte/offset smoke test, not a fuzzing claim.
    let mut seed = 7u32;
    for length in 0..256 {
        let mut bytes = vec![0u8; length];
        for byte in &mut bytes {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *byte = seed as u8;
        }
        let _ = png_dimensions(&bytes);
        let _ = dib_layout(&bytes, ClipboardSource::Dib);
        let _ = decode_hdrop(&bytes);
        let _ = decode_unicode_text(&bytes);
    }
}

#[cfg(not(windows))]
#[test]
fn other_platforms_report_unverified_unsupported_instead_of_success() {
    let mut clipboard = ClipboardAdapter::new();
    assert!(matches!(
        clipboard.read_image_or_file(),
        Err(ClipboardError::UnsupportedPlatform)
    ));
    assert!(matches!(
        clipboard.copy_pixels(&png_rgba()),
        Err(ClipboardError::UnsupportedPlatform)
    ));
    assert!(matches!(
        clipboard.copy_text("test"),
        Err(ClipboardError::UnsupportedPlatform)
    ));
}

#[test]
fn clipboard_png_applies_exif_orientation_without_retaining_file_identity() {
    // Little-endian TIFF with the single SHORT Orientation=6 entry.
    let exif = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder.set_exif_metadata(exif).unwrap();
    encoder
        .write_image(
            &[255, 0, 0, 1, 0, 0, 255, 128],
            2,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert_eq!(result.pixels.dimensions(), (1, 2));
    assert_eq!(result.pixels.as_raw(), &[255, 0, 0, 1, 0, 0, 255, 128]);
    assert_eq!(result.source, ClipboardSource::Png);
}

#[test]
fn clipboard_png_palette_transparency_expands_to_exact_rgba() {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 3, 1);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(vec![255, 0, 0, 0, 255, 0, 0, 0, 255]);
    encoder.set_trns(vec![0, 128, 255]);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[0, 1, 2])
        .unwrap();
    let result = decode_png(&bytes).unwrap();
    assert_eq!(
        result.pixels.as_raw(),
        &[255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255]
    );
}

#[test]
fn cancellation_before_copy_publication_never_calls_the_os_mutation() {
    let cancel = crate::Cancellation::default();
    cancel.cancel();
    let mut called = false;
    let result = publish_copy(&cancel, || {
        called = true;
        Ok(())
    });
    assert!(matches!(result, Err(ClipboardError::Cancelled)));
    assert!(!called);
}

#[test]
fn cancellation_after_copy_publication_reports_real_success() {
    let cancel = crate::Cancellation::default();
    let result = publish_copy(&cancel, || {
        cancel.cancel();
        Ok("copied")
    });
    assert_eq!(result.unwrap(), "copied");
    assert!(cancel.check().is_err());
    assert!(publish_copy(&cancel, || Ok(())).is_err());
}

#[test]
fn publication_preserves_partial_native_failure_even_if_cancel_arrives() {
    let cancel = crate::Cancellation::default();
    let result: ClipboardResult<()> = publish_copy(&cancel, || {
        cancel.cancel();
        Err(ClipboardError::WriteFailed {
            operation: "SetClipboardData",
            code: 8,
        })
    });
    assert!(matches!(
        result,
        Err(ClipboardError::WriteFailed {
            operation: "SetClipboardData",
            code: 8
        })
    ));
}
