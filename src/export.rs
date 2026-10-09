use crate::{Cancellation, Result, ToolError};
use image::{
    ImageDecoder, ImageEncoder, ImageFormat, ImageReader, RgbImage, RgbaImage,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportFormat {
    Png,
    Jpeg,
}
#[derive(Clone)]
pub struct ExportOptions {
    pub destination: PathBuf,
    pub format: ExportFormat,
    pub quality: u8,
    pub matte: [u8; 3],
}

struct CancellableWriter<'a, W> {
    inner: W,
    cancel: &'a Cancellation,
}
impl<W: Write> Write for CancellableWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.cancel
            .check()
            .map_err(|_| io::Error::other("cancelled"))?;
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn flatten(image: &RgbaImage, matte: [u8; 3]) -> RgbImage {
    flatten_rows(image, matte, || Ok(())).expect("infallible preview conversion")
}

fn flatten_rows(
    image: &RgbaImage,
    matte: [u8; 3],
    mut check: impl FnMut() -> Result<()>,
) -> Result<RgbImage> {
    check()?;
    let mut output = RgbImage::new(image.width(), image.height());
    for (source, destination) in image.rows().zip(output.rows_mut()) {
        // A JPEG export can be cancelled before its first encoder write.
        check()?;
        for (p, out) in source.zip(destination) {
            let a = u32::from(p[3]);
            *out = image::Rgb(std::array::from_fn(|c| {
                ((u32::from(p[c]) * a + u32::from(matte[c]) * (255 - a) + 127) / 255) as u8
            }));
        }
    }
    Ok(output)
}

pub fn save_new(
    image: &RgbaImage,
    original: &Path,
    options: &ExportOptions,
    cancel: &Cancellation,
) -> Result<PathBuf> {
    save_new_using(image, original, options, cancel, |file, icc| {
        encode(file, image, options, icc, cancel)
    })
}

fn save_new_using(
    image: &RgbaImage,
    original: &Path,
    options: &ExportOptions,
    cancel: &Cancellation,
    encoder: impl FnOnce(&mut std::fs::File, &[u8]) -> Result<()>,
) -> Result<PathBuf> {
    cancel.check()?;
    crate::pipeline::validate_dimensions(image.width(), image.height())?;
    let _reservation =
        crate::memory::reserve(image.as_raw().len() as u64 * 4 + crate::memory::OVERHEAD)?;
    let destination = &options.destination;
    if destination.exists() {
        return Err(ToolError::Invalid(
            "目标名称已存在，请选择新名称；不会覆盖任何文件".into(),
        ));
    }
    if destination == original {
        return Err(ToolError::Invalid("不能覆盖原图，请导出新副本".into()));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // tempfile creates exclusively in the same directory, allowing an atomic no-clobber commit.
    let mut temp = tempfile::Builder::new()
        .prefix(".light-image-export-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    let icc = moxcms::ColorProfile::new_srgb()
        .encode()
        .map_err(|e| ToolError::Invalid(format!("输出 sRGB ICC 失败：{e}")))?;
    let encoded = encoder(temp.as_file_mut(), &icc);
    cancel.check()?;
    encoded?;
    temp.as_file_mut().sync_all()?;
    cancel.check()?;
    validate_encoded(temp.path(), image, options.format, &icc)?;
    publish_validated(temp, destination, cancel)
}

fn publish_validated(
    temp: tempfile::NamedTempFile,
    destination: &Path,
    cancel: &Cancellation,
) -> Result<PathBuf> {
    cancel.publish(|| {
        temp.persist_noclobber(destination)
            .map_err(|e| ToolError::Io(e.error))?;
        Ok(destination.to_owned())
    })
}

fn encode(
    file: &mut std::fs::File,
    image: &RgbaImage,
    options: &ExportOptions,
    icc: &[u8],
    cancel: &Cancellation,
) -> Result<()> {
    let writer = CancellableWriter {
        inner: file,
        cancel,
    };
    match options.format {
        ExportFormat::Png => {
            let mut encoder = PngEncoder::new(writer);
            encoder
                .set_icc_profile(icc.to_vec())
                .map_err(image::ImageError::Unsupported)?;
            encoder.write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
        }
        ExportFormat::Jpeg => {
            let rgb = flatten_rows(image, options.matte, || cancel.check())?;
            cancel.check()?;
            let mut encoder = JpegEncoder::new_with_quality(writer, options.quality.clamp(1, 100));
            encoder
                .set_icc_profile(icc.to_vec())
                .map_err(image::ImageError::Unsupported)?;
            encoder.write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
        }
    }?;
    Ok(())
}

fn validate_encoded(
    path: &Path,
    source: &RgbaImage,
    format: ExportFormat,
    icc: &[u8],
) -> Result<()> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let expected = if format == ExportFormat::Png {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    if reader.format() != Some(expected) {
        return Err(ToolError::Invalid("导出回读格式不符，未发布文件".into()));
    }
    let mut decoder = reader.into_decoder()?;
    if decoder.dimensions() != source.dimensions()
        || decoder.orientation()? != image::metadata::Orientation::NoTransforms
        || decoder.icc_profile()?.as_deref() != Some(icc)
        || decoder.exif_metadata()?.is_some()
        || decoder.xmp_metadata()?.is_some()
    {
        return Err(ToolError::Invalid(
            "导出回读尺寸/色彩/元数据校验失败，未发布文件".into(),
        ));
    }
    let decoded = image::DynamicImage::from_decoder(decoder)?;
    if format == ExportFormat::Png && decoded.to_rgba8() != *source {
        return Err(ToolError::Invalid("PNG 回读像素不符，未发布文件".into()));
    }
    Ok(())
}

pub fn suggest_path(original: &Path, format: ExportFormat) -> PathBuf {
    let ext = if format == ExportFormat::Png {
        "png"
    } else {
        "jpg"
    };
    let stem = original.file_stem().unwrap_or_default().to_string_lossy();
    let parent = original.parent().unwrap_or(Path::new("."));
    for i in 0..10_000 {
        let suffix = if i == 0 {
            "-edited".into()
        } else {
            format!("-edited-{i}")
        };
        let path = parent.join(format!("{stem}{suffix}.{ext}"));
        if !path.exists() {
            return path;
        }
    }
    parent.join(format!("{stem}-edited-{}.{}", std::process::id(), ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jpeg_flatten_checks_cancellation_before_allocation_and_each_row() {
        let pixels = RgbaImage::from_fn(23, 17, |x, y| image::Rgba([x as u8, y as u8, 150, 128]));
        let mut checks = 0;
        let result = flatten_rows(&pixels, [240; 3], || {
            checks += 1;
            if checks == 4 {
                Err(ToolError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert_eq!(checks, 4);
        let output = flatten_rows(&pixels, [240; 3], || Ok(())).unwrap();
        for (source, actual) in pixels.pixels().zip(output.pixels()) {
            let expected: [u8; 3] = std::array::from_fn(|c| {
                ((u32::from(source[c]) * 128 + 240 * 127 + 127) / 255) as u8
            });
            assert_eq!(actual.0, expected);
        }
        assert_eq!(output, flatten(&pixels, [240; 3]));
    }
    #[test]
    fn cancelled_encoder_unwinds_only_its_owned_zero_or_partial_temporary() {
        for contents in [b"".as_slice(), b"partial encoding".as_slice()] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original.png");
            let pixels = RgbaImage::from_pixel(10, 10, image::Rgba([10, 20, 30, 128]));
            pixels.save(&original).unwrap();
            let before = std::fs::read(&original).unwrap();
            let unrelated = dir.path().join(".light-image-export-unrelated.tmp");
            std::fs::write(&unrelated, b"unrelated").unwrap();
            let destination = dir.path().join("unfinished.jpg");
            let options = ExportOptions {
                destination: destination.clone(),
                format: ExportFormat::Jpeg,
                quality: 90,
                matte: [255; 3],
            };
            let cancel = Cancellation::default();
            let token = cancel.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
            let original_path = original.clone();
            let worker = std::thread::spawn(move || {
                save_new_using(&pixels, &original_path, &options, &token, |file, _| {
                    file.write_all(contents)?;
                    ready_tx.send(()).unwrap();
                    let started = std::time::Instant::now();
                    while token.check().is_ok() {
                        assert!(started.elapsed() < std::time::Duration::from_secs(5));
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    token.check()
                })
            });
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
            cancel.cancel();
            assert!(matches!(worker.join().unwrap(), Err(ToolError::Cancelled)));
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&original).unwrap(), before);
            assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }
    #[test]
    fn cancellation_before_final_publication_removes_validated_temp_and_never_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        let pixels = RgbaImage::from_pixel(10, 10, image::Rgba([10, 20, 30, 128]));
        pixels.save(&original).unwrap();
        let original_bytes = std::fs::read(&original).unwrap();
        let options = ExportOptions {
            destination: dir.path().join("copy.png"),
            format: ExportFormat::Png,
            quality: 90,
            matte: [255; 3],
        };
        let cancel = Cancellation::default();
        let mut temp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let icc = moxcms::ColorProfile::new_srgb().encode().unwrap();
        encode(temp.as_file_mut(), &pixels, &options, &icc, &cancel).unwrap();
        temp.as_file_mut().sync_all().unwrap();
        validate_encoded(temp.path(), &pixels, options.format, &icc).unwrap();
        cancel.cancel();
        assert!(matches!(
            publish_validated(temp, &options.destination, &cancel),
            Err(ToolError::Cancelled)
        ));
        assert!(!options.destination.exists());
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn publication_gate_winner_reports_real_success_when_another_thread_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let pixels = RgbaImage::from_pixel(10, 10, image::Rgba([10, 20, 30, 128]));
        let options = ExportOptions {
            destination: dir.path().join("copy.png"),
            format: ExportFormat::Png,
            quality: 90,
            matte: [255; 3],
        };
        let cancel = Cancellation::default();
        let mut temp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let icc = moxcms::ColorProfile::new_srgb().encode().unwrap();
        encode(temp.as_file_mut(), &pixels, &options, &icc, &cancel).unwrap();
        validate_encoded(temp.path(), &pixels, options.format, &icc).unwrap();
        let concurrent_cancel = cancel.clone();
        let result = cancel
            .publish(|| {
                std::thread::spawn(move || concurrent_cancel.cancel())
                    .join()
                    .unwrap();
                temp.persist_noclobber(&options.destination)
                    .map_err(|error| ToolError::Io(error.error))?;
                Ok(options.destination.clone())
            })
            .unwrap();
        assert_eq!(result, options.destination);
        assert_eq!(image::open(&result).unwrap().into_rgba8(), pixels);
        cancel.cancel();
        assert!(matches!(cancel.check(), Err(ToolError::Cancelled)));
        assert!(matches!(
            crate::edit::apply(&pixels, &crate::edit::EditOp::Rotate90, &cancel),
            Err(ToolError::Cancelled)
        ));
        assert!(cancel.publish(|| Ok(())).is_err());
    }
    #[test]
    fn partial_disk_full_cancel_and_panic_never_publish_and_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        let pixels = RgbaImage::from_pixel(10, 10, image::Rgba([10, 20, 30, 128]));
        pixels.save(&original).unwrap();
        let before = std::fs::read(&original).unwrap();
        let options = ExportOptions {
            destination: dir.path().join("copy.png"),
            format: ExportFormat::Png,
            quality: 90,
            matte: [255; 3],
        };
        let result = save_new_using(
            &pixels,
            &original,
            &options,
            &Cancellation::default(),
            |file, _| {
                file.write_all(b"partial encoding")?;
                Err(ToolError::Io(io::Error::from_raw_os_error(112)))
            },
        );
        assert!(result.is_err());
        let token = Cancellation::default();
        let result = save_new_using(&pixels, &original, &options, &token, |file, _| {
            file.write_all(b"partial encoding")?;
            token.cancel();
            Ok(())
        });
        assert!(matches!(result, Err(ToolError::Cancelled)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            save_new_using(
                &pixels,
                &original,
                &options,
                &Cancellation::default(),
                |file, _| {
                    file.write_all(b"partial encoding")?;
                    panic!("injected encoding panic")
                },
            )
        }));
        assert!(result.is_err());
        assert!(!options.destination.exists());
        assert_eq!(std::fs::read(&original).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        save_new(&pixels, &original, &options, &Cancellation::default()).unwrap();
        assert!(options.destination.exists());
    }
}
