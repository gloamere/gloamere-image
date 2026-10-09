use image::{ImageEncoder, Rgba, RgbaImage, codecs::png::PngEncoder};
use light_image_tool::{
    Cancellation, ToolError,
    edit::{self, EditOp},
    export::{self, ExportFormat, ExportOptions},
    pipeline,
};
use std::{fs, sync::Arc};

fn sample() -> RgbaImage {
    RgbaImage::from_fn(24, 16, |x, y| {
        Rgba([
            (x * 10) as u8,
            (y * 15) as u8,
            80,
            if x < 8 {
                0
            } else if x < 16 {
                128
            } else {
                255
            },
        ])
    })
}
fn png(path: &std::path::Path, pixels: &RgbaImage) {
    pixels
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}
fn options(path: std::path::PathBuf, format: ExportFormat) -> ExportOptions {
    ExportOptions {
        destination: path,
        format,
        quality: 95,
        matte: [255; 3],
    }
}

#[test]
fn alpha_roundtrip_and_original_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("中文 原图.png");
    let target = dir.path().join("新副本.png");
    png(&source, &sample());
    let before = fs::read(&source).unwrap();
    let image = pipeline::load(&source, &Cancellation::default()).unwrap();
    assert!(image.has_alpha);
    export::save_new(
        &image.pixels,
        &source,
        &options(target.clone(), ExportFormat::Png),
        &Cancellation::default(),
    )
    .unwrap();
    let reopened = pipeline::load(&target, &Cancellation::default()).unwrap();
    assert_eq!(*reopened.pixels, *image.pixels);
    assert_eq!(before, fs::read(&source).unwrap());
}

#[test]
fn all_eight_exif_orientations_are_applied_once() {
    let dir = tempfile::tempdir().unwrap();
    let base = RgbaImage::from_fn(2, 3, |x, y| Rgba([(y * 2 + x + 1) as u8, 0, 0, 255]));
    let expected: [&[u8]; 8] = [
        &[1, 2, 3, 4, 5, 6],
        &[2, 1, 4, 3, 6, 5],
        &[6, 5, 4, 3, 2, 1],
        &[5, 6, 3, 4, 1, 2],
        &[1, 3, 5, 2, 4, 6],
        &[5, 3, 1, 6, 4, 2],
        &[6, 4, 2, 5, 3, 1],
        &[2, 4, 6, 1, 3, 5],
    ];
    for orientation in 1..=8u8 {
        let path = dir.path().join(format!("exif-{orientation}.png"));
        let mut bytes = Vec::new();
        // TIFF little-endian, one SHORT Orientation tag. The library owns image container encoding.
        let mut exif = vec![
            b'I',
            b'I',
            42,
            0,
            8,
            0,
            0,
            0,
            1,
            0,
            0x12,
            1,
            3,
            0,
            1,
            0,
            0,
            0,
            orientation,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        let mut encoder = PngEncoder::new(&mut bytes);
        encoder
            .set_exif_metadata(std::mem::take(&mut exif))
            .unwrap();
        encoder
            .write_image(base.as_raw(), 2, 3, image::ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(&path, bytes).unwrap();
        let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
        assert_eq!(loaded.orientation, orientation);
        let actual: Vec<_> = loaded.pixels.pixels().map(|p| p[0]).collect();
        assert_eq!(
            actual,
            expected[(orientation - 1) as usize],
            "orientation {orientation}"
        );
        assert_eq!(
            loaded.pixels.dimensions(),
            if orientation >= 5 { (3, 2) } else { (2, 3) }
        );
        let exported = dir.path().join(format!("baked-{orientation}.png"));
        export::save_new(
            &loaded.pixels,
            &path,
            &options(exported.clone(), ExportFormat::Png),
            &Cancellation::default(),
        )
        .unwrap();
        let reopened = pipeline::load(&exported, &Cancellation::default()).unwrap();
        assert_eq!(*loaded.pixels, *reopened.pixels);
        assert_eq!(reopened.orientation, 1);
    }
}

#[test]
fn dimensions_and_format_are_bounded() {
    assert!(pipeline::validate_dimensions(0, 1).is_err());
    assert!(pipeline::validate_dimensions(32769, 1).is_err());
    assert!(pipeline::validate_dimensions(10000, 10000).is_err());
    assert!(pipeline::validate_dimensions(4000, 4000).is_ok());
    assert!(pipeline::validate_dimensions(4001, 4000).is_err());
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.png");
    fs::write(&bad, b"not an image").unwrap();
    assert!(pipeline::load(&bad, &Cancellation::default()).is_err());
    let disguised = dir.path().join("actual.png.jpg");
    png(&disguised, &sample());
    assert_eq!(
        pipeline::load(&disguised, &Cancellation::default())
            .unwrap()
            .format,
        image::ImageFormat::Png
    );
    let huge = dir.path().join("huge.png");
    let file = fs::File::create(&huge).unwrap();
    file.set_len(pipeline::MAX_INPUT_BYTES + 1).unwrap();
    assert!(pipeline::load(&huge, &Cancellation::default()).is_err());
}

#[test]
fn extreme_aspect_ratio_resize_is_rejected_before_float_intermediate() {
    let pixels = RgbaImage::new(32768, 1);
    assert!(
        edit::apply(
            &pixels,
            &EditOp::Resize {
                width: 1,
                height: 32768
            },
            &Cancellation::default()
        )
        .is_err()
    );
}

#[test]
fn animated_png_is_rejected_instead_of_silently_showing_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("animated.png");
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&[255, 0, 0, 255, 255, 0, 0, 255])
        .unwrap();
    writer
        .write_image_data(&[0, 255, 0, 255, 0, 255, 0, 255])
        .unwrap();
    writer.finish().unwrap();
    fs::write(&path, bytes).unwrap();
    let error = pipeline::load(&path, &Cancellation::default())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("APNG"));
}

#[test]
fn sixteen_bit_png_is_explicitly_reduced_and_original_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("16bit.png");
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            &[0, 0, 128, 0, 255, 255],
            3,
            1,
            image::ExtendedColorType::L16,
        )
        .unwrap();
    fs::write(&path, &bytes).unwrap();
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(loaded.warnings.iter().any(|w| w.contains("8 位")));
    assert_eq!(loaded.pixels.get_pixel(0, 0)[0], 0);
    assert_eq!(loaded.pixels.get_pixel(2, 0)[0], 255);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn truncated_png_fails_without_partial_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.png");
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(sample().as_raw(), 24, 16, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes.truncate(bytes.len() / 2);
    fs::write(&path, bytes).unwrap();
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
}

#[test]
fn editor_crop_rotate_and_rejects_invalid_input() {
    let image = sample();
    let token = Cancellation::default();
    let cropped = edit::apply(
        &image,
        &EditOp::Crop {
            x: 2,
            y: 3,
            width: 5,
            height: 6,
        },
        &token,
    )
    .unwrap();
    assert_eq!(cropped.dimensions(), (5, 6));
    assert_eq!(cropped.get_pixel(0, 0), image.get_pixel(2, 3));
    let rotated = edit::apply(&image, &EditOp::Rotate90, &token).unwrap();
    assert_eq!(rotated.dimensions(), (16, 24));
    let restored = edit::apply(&rotated, &EditOp::Rotate270, &token).unwrap();
    assert_eq!(restored, image);
    assert!(
        edit::apply(
            &image,
            &EditOp::Crop {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1
            },
            &token
        )
        .is_err()
    );
    assert!(
        edit::apply(
            &image,
            &EditOp::Resize {
                width: 0,
                height: 1
            },
            &token
        )
        .is_err()
    );
}

#[test]
fn invisible_colors_do_not_bleed_on_resize() {
    let image = RgbaImage::from_fn(2, 1, |x, _| {
        if x == 0 {
            Rgba([255, 0, 0, 0])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    let resized = edit::apply(
        &image,
        &EditOp::Resize {
            width: 10,
            height: 1,
        },
        &Cancellation::default(),
    )
    .unwrap();
    for p in resized.pixels().filter(|p| p[3] > 0) {
        assert_eq!(p[0], 0);
        assert!(p[2] > 240);
    }
}

#[test]
fn export_collision_cancel_and_missing_directory_preserve_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    png(&source, &sample());
    let before = fs::read(&source).unwrap();
    assert!(
        export::save_new(
            &sample(),
            &source,
            &options(source.clone(), ExportFormat::Png),
            &Cancellation::default()
        )
        .is_err()
    );
    assert_eq!(before, fs::read(&source).unwrap());
    let dest = dir.path().join("existing.png");
    fs::write(&dest, b"sentinel").unwrap();
    assert!(
        export::save_new(
            &sample(),
            &source,
            &options(dest.clone(), ExportFormat::Png),
            &Cancellation::default()
        )
        .is_err()
    );
    assert_eq!(fs::read(&dest).unwrap(), b"sentinel");
    let cancelled = dir.path().join("cancel.png");
    let token = Cancellation::default();
    token.cancel();
    assert!(matches!(
        export::save_new(
            &sample(),
            &source,
            &options(cancelled.clone(), ExportFormat::Png),
            &token
        ),
        Err(ToolError::Cancelled)
    ));
    assert!(!cancelled.exists());
    assert!(
        export::save_new(
            &sample(),
            &source,
            &options(dir.path().join("missing/a.png"), ExportFormat::Png),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(
        fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
}

#[test]
fn concurrent_exports_have_one_winner_without_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    let dest = dir.path().join("race.png");
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|i| {
            let barrier = barrier.clone();
            let source = source.clone();
            let dest = dest.clone();
            std::thread::spawn(move || {
                let pixels = RgbaImage::from_pixel(800, 800, Rgba([i * 60, 10, 20, 255]));
                barrier.wait();
                export::save_new(
                    &pixels,
                    &source,
                    &options(dest, ExportFormat::Png),
                    &Cancellation::default(),
                )
                .is_ok()
            })
        })
        .collect();
    let winners = threads
        .into_iter()
        .map(|t| usize::from(t.join().unwrap()))
        .sum::<usize>();
    assert_eq!(winners, 1);
    assert!(pipeline::load(&dest, &Cancellation::default()).is_ok());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn jpeg_flattens_alpha_with_explicit_matte() {
    let image = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 255, 0]));
    assert_eq!(
        *export::flatten(&image, [255; 3]).get_pixel(0, 0),
        image::Rgb([255; 3])
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matte.jpg");
    export::save_new(
        &image,
        &dir.path().join("source.png"),
        &options(path.clone(), ExportFormat::Jpeg),
        &Cancellation::default(),
    )
    .unwrap();
    let reopened = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(!reopened.has_alpha);
    assert!(reopened.pixels.get_pixel(0, 0)[0] > 250);
}

#[test]
fn icc_rgb_srgb_preserves_alpha_and_invalid_profile_is_not_verified() {
    let dir = tempfile::tempdir().unwrap();
    for (name, icc, valid) in [
        (
            "srgb",
            moxcms::ColorProfile::new_srgb().encode().unwrap(),
            true,
        ),
        ("invalid", vec![0; 200], false),
    ] {
        let path = dir.path().join(format!("{name}.png"));
        let pixels = sample();
        let mut bytes = Vec::new();
        let mut encoder = PngEncoder::new(&mut bytes);
        encoder.set_icc_profile(icc).unwrap();
        encoder
            .write_image(
                pixels.as_raw(),
                pixels.width(),
                pixels.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        fs::write(&path, bytes).unwrap();
        let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
        assert_eq!(loaded.color_managed, valid);
        assert!(loaded.has_alpha);
        assert_eq!(
            loaded.pixels.pixels().map(|p| p[3]).collect::<Vec<_>>(),
            pixels.pixels().map(|p| p[3]).collect::<Vec<_>>()
        );
        if valid {
            for (a, b) in loaded.pixels.pixels().zip(pixels.pixels()) {
                for c in 0..3 {
                    assert!(a[c].abs_diff(b[c]) <= 1);
                }
            }
        }
    }
}

#[test]
fn icc_display_p3_is_transformed_instead_of_merely_relabelled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p3.png");
    let pixels = RgbaImage::from_pixel(8, 8, Rgba([180, 100, 80, 128]));
    let mut bytes = Vec::new();
    let mut encoder = PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
        .unwrap();
    encoder
        .write_image(pixels.as_raw(), 8, 8, image::ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(&path, bytes).unwrap();
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(loaded.color_managed);
    assert_ne!(loaded.pixels.get_pixel(0, 0), pixels.get_pixel(0, 0));
    assert_eq!(loaded.pixels.get_pixel(0, 0)[3], 128);
    // Reference approximate Display-P3 -> sRGB result for this input, within quantization tolerance.
    let p = loaded.pixels.get_pixel(0, 0);
    assert!((185..=199).contains(&p[0]));
    assert!((89..=102).contains(&p[1]));
    assert!((69..=81).contains(&p[2]));
}

fn gray_linear_icc_case(sixteen_bit: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gray-alpha.png");
    let mut bytes = Vec::new();
    let mut encoder = PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(
            moxcms::ColorProfile::new_gray_with_gamma(1.0)
                .encode()
                .unwrap(),
        )
        .unwrap();
    if sixteen_bit {
        let samples = [0u16, 65535, 16384, 32768, 32768, 16384, 65535, 0];
        let native: Vec<u8> = samples.iter().flat_map(|v| v.to_ne_bytes()).collect();
        encoder
            .write_image(&native, 4, 1, image::ExtendedColorType::La16)
            .unwrap();
    } else {
        encoder
            .write_image(
                &[0, 255, 64, 128, 128, 64, 255, 0],
                4,
                1,
                image::ExtendedColorType::La8,
            )
            .unwrap();
    }
    fs::write(&path, &bytes).unwrap();
    let loaded = pipeline::load(&path, &Cancellation::default()).unwrap();
    assert!(loaded.color_managed);
    assert!(loaded.has_alpha);
    assert_eq!(
        loaded.warnings.iter().any(|w| w.contains("8 位")),
        sixteen_bit
    );
    for (pixel, (value, alpha)) in
        loaded
            .pixels
            .pixels()
            .zip([(0u8, 255), (137, 128), (188, 64), (255, 0)])
    {
        // Linear gray quarter/half intensity mapped to the standard sRGB transfer curve.
        for channel in 0..3 {
            assert!(
                pixel[channel].abs_diff(value) <= 2,
                "{pixel:?} expected {value}"
            );
        }
        assert_eq!(pixel[3], alpha);
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let output = dir.path().join("gray-srgb-copy.png");
    export::save_new(
        &loaded.pixels,
        &path,
        &options(output.clone(), ExportFormat::Png),
        &Cancellation::default(),
    )
    .unwrap();
    let reopened = pipeline::load(&output, &Cancellation::default()).unwrap();
    assert_eq!(loaded.pixels.as_raw(), reopened.pixels.as_raw());
}

#[test]
fn gray_linear_icc_preserves_alpha_and_srgb_export() {
    gray_linear_icc_case(false);
}

#[test]
fn gray_sixteen_bit_icc_reduces_original_luminance_before_srgb() {
    gray_linear_icc_case(true);
}

#[cfg(windows)]
#[test]
fn basename_and_windows_case_alias_have_one_current_sibling() {
    if std::env::var_os("LIGHT_IMAGE_TEST_BROWSE_CHILD").is_some() {
        for name in ["Beta-素材.png", "beta-素材.PNG"] {
            let path = std::path::Path::new(name);
            let loaded = pipeline::load(path, &Cancellation::default()).unwrap();
            let siblings = pipeline::sibling_images(path);
            assert_eq!(
                siblings.len(),
                3,
                "basename/current-directory or case alias duplicated asset: {siblings:?}"
            );
            assert_eq!(siblings.iter().filter(|p| p.as_path() == path).count(), 1);
            assert_eq!(siblings.iter().position(|p| p.as_path() == path), Some(1));
            assert_eq!(loaded.pixels.dimensions(), (24, 16));
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for name in ["alpha.png", "Beta-素材.png", "gamma.png"] {
        png(&dir.path().join(name), &sample());
    }
    let before = fs::read(dir.path().join("Beta-素材.png")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "basename_and_windows_case_alias_have_one_current_sibling",
            "--nocapture",
        ])
        .env("LIGHT_IMAGE_TEST_BROWSE_CHILD", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(dir.path().join("Beta-素材.png")).unwrap(), before);
}

#[test]
fn cancelled_browsing_does_not_return_a_partial_success() {
    let cancellation = Cancellation::default();
    cancellation.cancel();
    assert!(matches!(
        pipeline::sibling_images_cancellable(std::path::Path::new("never-open.png"), &cancellation),
        Err(ToolError::Cancelled)
    ));
}

#[cfg(windows)]
#[test]
fn long_windows_directory_paths_stay_within_the_browsing_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut parent = dir.path().to_path_buf();
    for level in 0..10 {
        parent.push(format!("{level:02}-{}", "p".repeat(170)));
    }
    fs::create_dir_all(&parent).unwrap();
    let source = parent.join("original-素材.png");
    png(&source, &sample());
    let before = fs::read(&source).unwrap();
    for index in 0..2500 {
        fs::write(parent.join(format!("asset-{index:04}.png")), []).unwrap();
    }
    let siblings = pipeline::sibling_images_cancellable(&source, &Cancellation::default()).unwrap();
    assert!(
        siblings.len() < 2501,
        "long paths should hit the 4MiB list budget"
    );
    assert!(
        siblings.iter().map(|p| p.as_os_str().len()).sum::<usize>()
            <= pipeline::MAX_SIBLING_PATH_BYTES
    );
    assert_eq!(siblings.iter().filter(|p| *p == &source).count(), 1);
    assert_eq!(fs::read(&source).unwrap(), before);
}
