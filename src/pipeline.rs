use crate::{Cancellation, Result, ToolError};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufRead, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 16_000_000;
pub const MAX_DIMENSION: u32 = 32768;
pub const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ICC_BYTES: usize = 4 * 1024 * 1024;
const READ_CHECK_BYTES: usize = 64 * 1024;

struct CancellableReader<'a> {
    inner: Cursor<&'a [u8]>,
    cancel: &'a Cancellation,
}
impl<'a> CancellableReader<'a> {
    fn new(bytes: &'a [u8], cancel: &'a Cancellation) -> Self {
        Self {
            inner: Cursor::new(bytes),
            cancel,
        }
    }
    fn check(&self) -> io::Result<()> {
        // Interrupted is retried by read_exact/read_to_end and cannot represent terminal cancellation.
        self.cancel
            .check()
            .map_err(|_| io::Error::other("图片读取已取消"))
    }
}
impl Read for CancellableReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let length = output.len().min(READ_CHECK_BYTES);
        self.inner.read(&mut output[..length])
    }
}
impl BufRead for CancellableReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.check()?;
        let bytes = self.inner.fill_buf()?;
        Ok(&bytes[..bytes.len().min(READ_CHECK_BYTES)])
    }
    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}
impl Seek for CancellableReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.inner.seek(from)
    }
}

#[derive(Clone)]
pub struct LoadedImage {
    pub animation: Option<Arc<crate::animation::AnimationSource>>,
    pub icon: Option<Arc<crate::icon_container::IconDirectory>>,
    pub path: PathBuf,
    pub pixels: Arc<crate::memory::Pixels>,
    pub format: ImageFormat,
    pub file_bytes: u64,
    pub has_alpha: bool,
    pub orientation: u8,
    pub color_status: String,
    pub color_managed: bool,
    pub source_sha256: String,
    pub warnings: Vec<String>,
    pub load_ms: u128,
}

pub fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(ToolError::Invalid(format!(
            "图片尺寸超出安全限制：{width} × {height}（最多 1600 万像素）"
        )));
    }
    Ok(())
}

pub fn load(path: &Path, cancel: &Cancellation) -> Result<LoadedImage> {
    let result = (|| {
        cancel.check()?;
        let mut file = File::open(path)?;
        load_open_file(path, &mut file, cancel)
    })();
    cancel.check()?;
    result
}

pub fn load_open_file(path: &Path, file: &mut File, cancel: &Cancellation) -> Result<LoadedImage> {
    load_open_file_selected(path, file, IconSelection::Default, cancel)
}

#[derive(Clone, Copy)]
enum IconSelection {
    Default,
    Index(usize),
    Reload {
        dimensions: [u32; 2],
        bit_depth: u16,
    },
}

/// Re-read the current file, preserving an ICO size rather than its old directory position.
/// If that size was removed, the current default entry is used. Replacement files retain
/// normal content-based loading semantics.
pub fn reload_icon(
    path: &Path,
    dimensions: [u32; 2],
    bit_depth: u16,
    cancel: &Cancellation,
) -> Result<LoadedImage> {
    let result = (|| {
        cancel.check()?;
        let mut file = File::open(path)?;
        load_open_file_selected(
            path,
            &mut file,
            IconSelection::Reload {
                dimensions,
                bit_depth,
            },
            cancel,
        )
    })();
    cancel.check()?;
    result
}

/// Load one size from an ICO. The index refers to the original directory order.
pub fn load_icon_entry(path: &Path, index: usize, cancel: &Cancellation) -> Result<LoadedImage> {
    let result = (|| {
        cancel.check()?;
        let mut file = File::open(path)?;
        load_open_file_icon_entry(path, &mut file, index, cancel)
    })();
    cancel.check()?;
    result
}

pub fn load_open_file_icon_entry(
    path: &Path,
    file: &mut File,
    index: usize,
    cancel: &Cancellation,
) -> Result<LoadedImage> {
    load_open_file_selected(path, file, IconSelection::Index(index), cancel)
}

fn load_open_file_selected(
    path: &Path,
    file: &mut File,
    selection: IconSelection,
    cancel: &Cancellation,
) -> Result<LoadedImage> {
    let result = (|| {
        let started = Instant::now();
        let bytes = Arc::new(read_snapshot_file(file, cancel)?);
        let format = image::guess_format(&bytes)?;
        if matches!(selection, IconSelection::Index(_)) && format != ImageFormat::Ico {
            return Err(ToolError::Invalid("当前文件不是 ICO 图片".into()));
        }
        if let Some(animated) =
            crate::animation::try_load(bytes.clone(), path, format, cancel, started)?
        {
            return Ok(animated);
        }
        decode(&bytes, path, selection, cancel, started)
    })();
    cancel.check()?;
    result
}

pub fn read_snapshot(path: &Path, cancel: &Cancellation) -> Result<crate::memory::Snapshot> {
    cancel.check()?;
    let mut file = File::open(path)?;
    read_snapshot_file(&mut file, cancel)
}

fn read_snapshot_file(file: &mut File, cancel: &Cancellation) -> Result<crate::memory::Snapshot> {
    cancel.check()?;
    file.seek(SeekFrom::Start(0))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ToolError::Invalid("请选择普通图片文件".into()));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(ToolError::Invalid("文件超过 64 MiB 限制".into()));
    }
    let mut reservation = crate::memory::reserve((meta.len() * 2).max(64 * 1024))?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if (bytes.len() + count) as u64 > MAX_INPUT_BYTES {
            return Err(ToolError::Invalid("读取期间文件超过大小限制".into()));
        }
        let wanted = bytes.len() + count;
        if wanted > bytes.capacity() {
            reservation.resize((wanted as u64 * 2).max(64 * 1024))?;
            bytes
                .try_reserve_exact(wanted - bytes.len())
                .map_err(|_| ToolError::Invalid("快照内存分配失败".into()))?;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    cancel.check()?;
    crate::memory::Snapshot::from_reserved(bytes, reservation)
}

fn decode(
    bytes: &[u8],
    path: &Path,
    selection: IconSelection,
    cancel: &Cancellation,
    started: Instant,
) -> Result<LoadedImage> {
    let mut reservation = crate::memory::reserve(MAX_DECODE_BYTES)?;
    // Never trust the extension. Restrict the accepted content formats explicitly.
    cancel.check()?;
    let format = image::guess_format(bytes)?;
    if !matches!(
        format,
        ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Ico
    ) {
        return Err(ToolError::Invalid(
            "当前版本支持 JPEG、PNG、GIF、WebP、BMP 和 ICO".into(),
        ));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let icon = if format == ImageFormat::Ico {
        let mut directory = crate::icon_container::IconDirectory::parse(bytes, cancel)?;
        match selection {
            IconSelection::Default => {}
            IconSelection::Index(index) => directory.select(Some(index))?,
            IconSelection::Reload {
                dimensions,
                bit_depth,
            } => directory.select_matching(dimensions, bit_depth),
        }
        Some(directory)
    } else {
        None
    };
    // PNG entries retain the normal PNG color-management path. DIB entries use the ICO
    // decoder, which also applies the legacy one-bit transparency mask.
    let normalized;
    let (decode_bytes, decode_format) = if let Some(icon) = &icon {
        let payload = icon.payload(bytes);
        if icon.entries[icon.selected_index].encoding == crate::icon_container::IconEncoding::Png {
            (payload, ImageFormat::Png)
        } else {
            reservation.resize(MAX_DECODE_BYTES + payload.len() as u64 + 22)?;
            normalized = icon.single_entry(bytes, cancel)?;
            (normalized.as_slice(), ImageFormat::Ico)
        }
    } else {
        (bytes, format)
    };
    let mut reader =
        ImageReader::with_format(CancellableReader::new(decode_bytes, cancel), decode_format);
    if decode_format == ImageFormat::Png
        && image::codecs::png::PngDecoder::with_limits(
            CancellableReader::new(decode_bytes, cancel),
            limits.clone(),
        )?
        .is_apng()?
    {
        return Err(ToolError::Invalid(
            "当前版本不支持动画 PNG（APNG）；请使用静态 PNG".into(),
        ));
    }
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    validate_dimensions(w, h)?;
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err(ToolError::Invalid("解码内存超过限制".into()));
    }
    reservation.resize(
        crate::memory::decode_estimate(0, decoder.total_bytes(), w, h)
            + if decode_format == ImageFormat::Ico {
                decode_bytes.len() as u64
            } else {
                0
            },
    )?;
    let mut warnings = Vec::new();
    let orientation = match decoder.orientation() {
        Ok(o) => o,
        Err(e) => {
            warnings.push(format!("方向元数据无法读取：{e}"));
            image::metadata::Orientation::NoTransforms
        }
    };
    let icc = match decoder.icc_profile() {
        Ok(p) => p,
        Err(e) => {
            warnings.push(format!("ICC 无法读取：{e}"));
            None
        }
    };
    let source_type = decoder.color_type();
    cancel.check()?;
    let mut decoded = if decode_format == ImageFormat::Png {
        drop(decoder);
        png_pixels(decode_bytes, cancel)?
    } else {
        DynamicImage::from_decoder(decoder)?
    };
    cancel.check()?;
    decoded.apply_orientation(orientation);
    if source_type.bits_per_pixel() > u16::from(source_type.channel_count()) * 8 {
        warnings.push("显示与导出副本为 8 位；原始高位深文件保持不变".into());
    }
    let bitmap_profile = if format == ImageFormat::Bmp {
        bitmap_icc(bytes, true, &mut warnings)
    } else if let Some(icon) = &icon {
        if decode_format == ImageFormat::Ico {
            bitmap_icc(icon.payload(bytes), false, &mut warnings)
        } else {
            None
        }
    } else {
        None
    };
    let prepared_icc = bitmap_profile.or(icc.as_deref()).map(parse_icc);
    // Gray ICC must use the original decoded luminance before an 8-bit RGBA conversion.
    let gray = prepared_icc
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .filter(|profile| profile.color_space == DataColorSpace::Gray)
        .map(|_| decoded.to_luma_alpha8());
    let mut rgba = decoded.into_rgba8();
    let color_status: String = if let Some(prepared) = prepared_icc {
        match prepared.and_then(|profile| transform_icc(&mut rgba, &profile, gray.as_ref(), cancel))
        {
            Ok(()) => "ICC 已转换为 sRGB".into(),
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Err(e) => {
                warnings.push(e.to_string());
                "ICC 未完整处理；颜色可能不准确".into()
            }
        }
    } else if !warnings.iter().any(|s| s.starts_with("ICC")) {
        "无 ICC，按 sRGB 显示".into()
    } else {
        "ICC 核验不完整".into()
    };
    let mut has_alpha = false;
    for row in rgba.rows() {
        cancel.check()?;
        if row.into_iter().any(|pixel| pixel[3] < 255) {
            has_alpha = true;
            break;
        }
    }
    cancel.check()?;
    let color_managed = !color_status.contains("未完整") && !color_status.contains("核验不完整");
    Ok(LoadedImage {
        animation: None,
        icon: icon.map(Arc::new),
        path: path.to_path_buf(),
        pixels: Arc::new(crate::memory::Pixels::from_reserved(
            rgba,
            &mut reservation,
        )?),
        format,
        file_bytes: bytes.len() as u64,
        has_alpha,
        orientation: orientation.to_exif(),
        color_status,
        color_managed,
        source_sha256: source_hash(bytes, cancel)?,
        warnings,
        load_ms: started.elapsed().as_millis(),
    })
}

fn source_hash(bytes: &[u8], cancel: &Cancellation) -> Result<String> {
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(READ_CHECK_BYTES) {
        cancel.check()?;
        digest.update(chunk);
    }
    Ok(format!("{:x}", digest.finalize()))
}

// image's BMP decoder does not expose V4/V5 color metadata. Read only the bounded
// embedded profile; linked profiles never trigger filesystem access.
fn bitmap_icc<'a>(
    bytes: &'a [u8],
    file_header: bool,
    warnings: &mut Vec<String>,
) -> Option<&'a [u8]> {
    let base = if file_header { 14 } else { 0 };
    let header = bytes.get(base..base + 4)?;
    let header = u32::from_le_bytes(header.try_into().unwrap());
    if !matches!(header, 108 | 124) || bytes.len() < base + header as usize {
        return None;
    }
    let color_space = u32::from_le_bytes(bytes[base + 56..base + 60].try_into().unwrap());
    match color_space {
        0x7352_4742 | 0x5769_6e20 => None, // LCS_sRGB / LCS_WINDOWS_COLOR_SPACE
        0x4d42_4544 if header == 124 => {
            let offset =
                u32::from_le_bytes(bytes[base + 112..base + 116].try_into().unwrap()) as usize;
            let length =
                u32::from_le_bytes(bytes[base + 116..base + 120].try_into().unwrap()) as usize;
            if offset < 124 || length == 0 || length > MAX_ICC_BYTES {
                warnings.push("ICC：BMP 内嵌颜色配置范围无效或超过 4 MiB".into());
                return None;
            }
            let profile = base.checked_add(offset).and_then(|start| {
                start
                    .checked_add(length)
                    .and_then(|end| bytes.get(start..end))
            });
            if profile.is_none() {
                warnings.push("ICC：BMP 内嵌颜色配置已截断".into());
            }
            profile
        }
        0x4c49_4e4b => {
            warnings.push("ICC：BMP 使用外部颜色配置，未读取外部文件；颜色可能不准确".into());
            None
        }
        _ => {
            warnings.push("ICC：BMP 校准颜色空间尚不支持；颜色可能不准确".into());
            None
        }
    }
}

fn png_error(error: png::DecodingError) -> ToolError {
    ToolError::Invalid(format!("PNG 解码失败：{error}"))
}

fn png_pixels(bytes: &[u8], cancel: &Cancellation) -> Result<DynamicImage> {
    let mut decoder = png::Decoder::new_with_limits(
        CancellableReader::new(bytes, cancel),
        png::Limits {
            bytes: MAX_DECODE_BYTES as usize,
        },
    );
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(png_error)?;
    let (width, height) = (reader.info().width, reader.info().height);
    validate_dimensions(width, height)?;
    if reader.info().animation_control.is_some() {
        return Err(ToolError::Invalid(
            "当前版本不支持动画 PNG（APNG）；请使用静态 PNG".into(),
        ));
    }
    let (color, depth) = reader.output_color_type();
    let length = reader
        .output_buffer_size()
        .filter(|length| *length as u64 <= MAX_DECODE_BYTES)
        .ok_or_else(|| ToolError::Invalid("PNG 解码内存超过限制".into()))?;
    let mut pixels = vec![0; length];
    let stride = length / height as usize;
    let bits_per_pixel = u8::try_from(color.samples() * depth as usize)
        .map_err(|_| ToolError::Invalid("PNG 像素类型无效".into()))?;
    let mut line = 0;
    loop {
        cancel.check()?;
        let Some(row) = reader.next_interlaced_row().map_err(png_error)? else {
            break;
        };
        match row.interlace() {
            png::InterlaceInfo::Adam7(info) => {
                png::expand_interlaced_row(&mut pixels, stride, row.data(), info, bits_per_pixel);
            }
            png::InterlaceInfo::Null(_) => {
                let destination = pixels
                    .get_mut(line * stride..(line + 1) * stride)
                    .filter(|destination| destination.len() == row.data().len())
                    .ok_or_else(|| ToolError::Invalid("PNG 行尺寸无效".into()))?;
                destination.copy_from_slice(row.data());
                line += 1;
            }
        }
    }
    cancel.check()?;
    macro_rules! image {
        ($variant:ident, $pixels:expr) => {
            image::ImageBuffer::from_raw(width, height, $pixels)
                .map(DynamicImage::$variant)
                .ok_or_else(|| ToolError::Invalid("PNG 像素尺寸无效".into()))
        };
    }
    if depth == png::BitDepth::Sixteen {
        let mut samples = Vec::with_capacity(pixels.len() / 2);
        for chunk in pixels.chunks(READ_CHECK_BYTES) {
            cancel.check()?;
            samples.extend(
                chunk
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|sample| u16::from_be_bytes(*sample)),
            );
        }
        drop(pixels);
        match color {
            png::ColorType::Grayscale => image!(ImageLuma16, samples),
            png::ColorType::GrayscaleAlpha => image!(ImageLumaA16, samples),
            png::ColorType::Rgb => image!(ImageRgb16, samples),
            png::ColorType::Rgba => image!(ImageRgba16, samples),
            _ => Err(ToolError::Invalid("PNG 像素类型不受支持".into())),
        }
    } else {
        match color {
            png::ColorType::Grayscale => image!(ImageLuma8, pixels),
            png::ColorType::GrayscaleAlpha => image!(ImageLumaA8, pixels),
            png::ColorType::Rgb => image!(ImageRgb8, pixels),
            png::ColorType::Rgba => image!(ImageRgba8, pixels),
            _ => Err(ToolError::Invalid("PNG 像素类型不受支持".into())),
        }
    }
}

struct PreviewPixels {
    image: DynamicImage,
    orientation: image::metadata::Orientation,
    icc: Option<Vec<u8>>,
    rgb_encoded: bool,
}

fn preview_workspace(
    reservation: &mut crate::memory::Reservation,
    width: u32,
    height: u32,
) -> Result<()> {
    validate_dimensions(width, height)?;
    // Codec scratch, raw/conversion or canvas buffers, and the bounded metadata/CMS allowance.
    reservation.resize(
        MAX_DECODE_BYTES + crate::memory::rgba_bytes(width, height) * 3 + crate::memory::OVERHEAD,
    )
}

/// A first-frame display preview. It does not compute provenance hashes or construct animation state.
pub fn thumbnail(path: &Path, edge: u32, cancel: &Cancellation) -> Result<RgbaImage> {
    let result = thumbnail_inner(path, edge, cancel);
    cancel.check()?;
    result
}

fn thumbnail_inner(path: &Path, edge: u32, cancel: &Cancellation) -> Result<RgbaImage> {
    if edge == 0 || edge > 256 {
        return Err(ToolError::Invalid("缩略图边长必须在 1 到 256 之间".into()));
    }
    cancel.check()?;
    let bytes = read_snapshot(path, cancel)?;
    let format = image::guess_format(&bytes)?;
    let mut reservation = crate::memory::reserve(MAX_DECODE_BYTES)?;
    let preview = match format {
        ImageFormat::Gif => gif_preview(&bytes, cancel, &mut reservation)?,
        ImageFormat::WebP => webp_preview(&bytes, cancel, &mut reservation)?,
        ImageFormat::Jpeg => match jpeg_preview(&bytes, edge, cancel, &mut reservation) {
            Ok(preview) => preview,
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Err(_) => static_preview(&bytes, format, cancel, &mut reservation)?,
        },
        ImageFormat::Png | ImageFormat::Bmp | ImageFormat::Ico => {
            static_preview(&bytes, format, cancel, &mut reservation)?
        }
        _ => {
            return Err(ToolError::Invalid(
                "当前版本支持 JPEG、PNG、GIF、WebP、BMP 和 ICO".into(),
            ));
        }
    };
    cancel.check()?;
    let mut reduced = if preview.image.width() > edge || preview.image.height() > edge {
        preview.image.thumbnail(edge, edge)
    } else {
        preview.image
    };
    cancel.check()?;
    reduced.apply_orientation(preview.orientation);
    let profile = preview
        .icc
        .as_deref()
        .and_then(|icc| parse_icc(icc).ok())
        .filter(|profile| !preview.rgb_encoded || profile.color_space == DataColorSpace::Rgb);
    let gray = profile
        .as_ref()
        .filter(|profile| profile.color_space == DataColorSpace::Gray)
        .map(|_| reduced.to_luma_alpha8());
    let mut rgba = reduced.into_rgba8();
    if let Some(profile) = profile {
        match transform_icc(&mut rgba, &profile, gray.as_ref(), cancel) {
            Ok(()) => {}
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            // Match the viewer's uncalibrated fallback when an ICC cannot be applied.
            Err(_) => {}
        }
    }
    cancel.check()?;
    Ok(rgba)
}

fn static_preview(
    bytes: &[u8],
    format: ImageFormat,
    cancel: &Cancellation,
    reservation: &mut crate::memory::Reservation,
) -> Result<PreviewPixels> {
    if format == ImageFormat::Ico {
        let directory = crate::icon_container::IconDirectory::parse(bytes, cancel)?;
        let payload = directory.payload(bytes);
        if directory.entries[directory.selected_index].encoding
            == crate::icon_container::IconEncoding::Png
        {
            return static_preview_decode(payload, ImageFormat::Png, cancel, reservation, 0);
        }
        reservation.resize(MAX_DECODE_BYTES + payload.len() as u64 + 22)?;
        let normalized = directory.single_entry(bytes, cancel)?;
        return static_preview_decode(
            &normalized,
            ImageFormat::Ico,
            cancel,
            reservation,
            normalized.len() as u64,
        );
    }
    static_preview_decode(bytes, format, cancel, reservation, 0)
}

fn static_preview_decode(
    bytes: &[u8],
    format: ImageFormat,
    cancel: &Cancellation,
    reservation: &mut crate::memory::Reservation,
    encoded_workspace: u64,
) -> Result<PreviewPixels> {
    let mut reader = ImageReader::with_format(CancellableReader::new(bytes, cancel), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    // Keep the selected DIB container admitted throughout the resize; releasing its
    // bytes and immediately reserving them again would undercount concurrent work.
    reservation.resize(
        MAX_DECODE_BYTES
            + crate::memory::rgba_bytes(width, height) * 3
            + crate::memory::OVERHEAD
            + encoded_workspace,
    )?;
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err(ToolError::Invalid("解码内存超过限制".into()));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut icc = decoder.icc_profile().ok().flatten();
    if format == ImageFormat::Bmp {
        icc = bitmap_icc(bytes, true, &mut Vec::new())
            .map(<[u8]>::to_vec)
            .or(icc);
    } else if format == ImageFormat::Ico {
        // The normalized single-entry container always starts its DIB at byte 22.
        icc = bitmap_icc(&bytes[22..], false, &mut Vec::new())
            .map(<[u8]>::to_vec)
            .or(icc);
    }
    cancel.check()?;
    let image = if format == ImageFormat::Png {
        drop(decoder);
        png_pixels(bytes, cancel)?
    } else {
        DynamicImage::from_decoder(decoder)?
    };
    Ok(PreviewPixels {
        image,
        orientation,
        icc,
        rgb_encoded: false,
    })
}

fn jpeg_preview(
    bytes: &[u8],
    edge: u32,
    cancel: &Cancellation,
    reservation: &mut crate::memory::Reservation,
) -> Result<PreviewPixels> {
    let mut decoder = jpeg_decoder::Decoder::new(CancellableReader::new(bytes, cancel));
    decoder.set_max_decoding_buffer_size(MAX_DECODE_BYTES as usize);
    decoder
        .read_info()
        .map_err(|error| ToolError::Invalid(format!("JPEG 预览解码失败：{error}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| ToolError::Invalid("JPEG 缺少尺寸信息".into()))?;
    preview_workspace(reservation, u32::from(info.width), u32::from(info.height))?;
    if !matches!(
        info.pixel_format,
        jpeg_decoder::PixelFormat::L8 | jpeg_decoder::PixelFormat::RGB24
    ) {
        return Err(ToolError::Invalid(
            "JPEG 预览像素类型需使用完整解码器".into(),
        ));
    }
    decoder
        .scale(edge as u16, edge as u16)
        .map_err(|error| ToolError::Invalid(format!("JPEG 预览缩小失败：{error}")))?;
    cancel.check()?;
    let pixels = decoder
        .decode()
        .map_err(|error| ToolError::Invalid(format!("JPEG 预览解码失败：{error}")))?;
    cancel.check()?;
    let info = decoder.info().unwrap();
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let image = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(width, height, pixels).map(DynamicImage::ImageLuma8)
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            image::RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
        }
        _ => None,
    }
    .ok_or_else(|| ToolError::Invalid("JPEG 预览像素尺寸无效".into()))?;
    let orientation = decoder
        .exif_data()
        .and_then(image::metadata::Orientation::from_exif_chunk)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    Ok(PreviewPixels {
        image,
        orientation,
        icc: decoder.icc_profile(),
        rgb_encoded: false,
    })
}

fn gif_preview(
    bytes: &[u8],
    cancel: &Cancellation,
    reservation: &mut crate::memory::Reservation,
) -> Result<PreviewPixels> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.check_frame_consistency(true);
    options.check_lzw_end_code(true);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        (MAX_ICC_BYTES as u64).try_into().unwrap(),
    ));
    let mut decoder = options
        .read_info(CancellableReader::new(bytes, cancel))
        .map_err(|error| ToolError::Invalid(format!("GIF 预览解码失败：{error}")))?;
    let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
    preview_workspace(reservation, width, height)?;
    let frame = decoder
        .next_frame_info()
        .map_err(|error| ToolError::Invalid(format!("GIF 首帧无效：{error}")))?
        .ok_or_else(|| ToolError::Invalid("GIF 没有可显示的帧".into()))?;
    let (left, top, frame_width, frame_height, interlaced) = (
        u32::from(frame.left),
        u32::from(frame.top),
        u32::from(frame.width),
        u32::from(frame.height),
        frame.interlaced,
    );
    validate_dimensions(frame_width, frame_height)?;
    if left + frame_width > width || top + frame_height > height {
        return Err(ToolError::Invalid("GIF 首帧超出画布".into()));
    }
    let mut local = vec![0; decoder.buffer_size()];
    if interlaced {
        decoder
            .read_into_buffer(&mut local)
            .map_err(|error| ToolError::Invalid(format!("GIF 首帧解码失败：{error}")))?;
    } else {
        for row in local.chunks_exact_mut(frame_width as usize * 4) {
            cancel.check()?;
            if !decoder
                .fill_buffer(row)
                .map_err(|error| ToolError::Invalid(format!("GIF 首帧解码失败：{error}")))?
            {
                return Err(ToolError::Invalid("GIF 首帧已截断".into()));
            }
        }
    }
    cancel.check()?;
    let mut canvas = RgbaImage::new(width, height);
    for (y, row) in local.chunks_exact(frame_width as usize * 4).enumerate() {
        cancel.check()?;
        let start = (((top as usize + y) * width as usize) + left as usize) * 4;
        canvas.as_flat_samples_mut().samples[start..start + row.len()].copy_from_slice(row);
    }
    let icc = decoder
        .icc_profile()
        .filter(|icc| icc.len() <= MAX_ICC_BYTES)
        .map(<[u8]>::to_vec);
    Ok(PreviewPixels {
        image: DynamicImage::ImageRgba8(canvas),
        orientation: image::metadata::Orientation::NoTransforms,
        icc,
        rgb_encoded: true,
    })
}

fn webp_preview(
    bytes: &[u8],
    cancel: &Cancellation,
    reservation: &mut crate::memory::Reservation,
) -> Result<PreviewPixels> {
    let first_frame = validate_webp_preview(bytes, cancel)?;
    let mut decoder = image_webp::WebPDecoder::new(CancellableReader::new(bytes, cancel))
        .map_err(|error| ToolError::Invalid(format!("WebP 预览解码失败：{error}")))?;
    decoder.set_memory_limit(MAX_ICC_BYTES);
    let (width, height) = decoder.dimensions();
    preview_workspace(reservation, width, height)?;
    let orientation = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .as_deref()
        .and_then(image::metadata::Orientation::from_exif_chunk)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = decoder.icc_profile().ok().flatten();
    if let Some(frame) = first_frame {
        // Decode the local bitstream alone. This preserves a transparent first-frame canvas
        // even when an animated file's global alpha flag is absent.
        let mut encoded = Vec::with_capacity(frame.payload.len() + 30);
        encoded.extend_from_slice(b"RIFF");
        encoded.extend_from_slice(&((frame.payload.len() + 22) as u32).to_le_bytes());
        encoded.extend_from_slice(b"WEBPVP8X");
        encoded.extend_from_slice(&10u32.to_le_bytes());
        encoded.extend_from_slice(&[if frame.alpha { 0x10 } else { 0 }, 0, 0, 0]);
        encoded.extend_from_slice(&(frame.width - 1).to_le_bytes()[..3]);
        encoded.extend_from_slice(&(frame.height - 1).to_le_bytes()[..3]);
        for chunk in frame.payload.chunks(READ_CHECK_BYTES) {
            cancel.check()?;
            encoded.extend_from_slice(chunk);
        }
        let mut local_decoder =
            image_webp::WebPDecoder::new(CancellableReader::new(&encoded, cancel))
                .map_err(|error| ToolError::Invalid(format!("WebP 首帧解码失败：{error}")))?;
        local_decoder.set_memory_limit(MAX_ICC_BYTES);
        let mut local = vec![
            0;
            local_decoder
                .output_buffer_size()
                .ok_or_else(|| ToolError::Invalid("WebP 首帧尺寸无效".into()))?
        ];
        local_decoder
            .read_image(&mut local)
            .map_err(|error| ToolError::Invalid(format!("WebP 首帧解码失败：{error}")))?;
        let local = if local_decoder.has_alpha() {
            image::RgbaImage::from_raw(frame.width, frame.height, local)
        } else {
            image::RgbImage::from_raw(frame.width, frame.height, local)
                .map(|image| DynamicImage::ImageRgb8(image).into_rgba8())
        }
        .ok_or_else(|| ToolError::Invalid("WebP 首帧像素尺寸无效".into()))?;
        let mut canvas = RgbaImage::new(width, height);
        for y in 0..frame.height {
            cancel.check()?;
            for x in 0..frame.width {
                let pixel = *local.get_pixel(x, y);
                if !frame.blend || pixel[3] != 0 {
                    canvas.put_pixel(frame.x + x, frame.y + y, pixel);
                }
            }
        }
        return Ok(PreviewPixels {
            image: DynamicImage::ImageRgba8(canvas),
            orientation,
            icc,
            rgb_encoded: true,
        });
    }
    let mut raw = vec![
        0;
        decoder
            .output_buffer_size()
            .ok_or_else(|| ToolError::Invalid("WebP 输出尺寸无效".into()))?
    ];
    cancel.check()?;
    decoder
        .read_image(&mut raw)
        .map_err(|error| ToolError::Invalid(format!("WebP 首帧解码失败：{error}")))?;
    let image = if decoder.has_alpha() {
        image::RgbaImage::from_raw(width, height, raw).map(DynamicImage::ImageRgba8)
    } else {
        image::RgbImage::from_raw(width, height, raw).map(DynamicImage::ImageRgb8)
    }
    .ok_or_else(|| ToolError::Invalid("WebP 像素尺寸无效".into()))?;
    Ok(PreviewPixels {
        image,
        orientation,
        icc,
        rgb_encoded: true,
    })
}

struct WebpPreviewFrame<'a> {
    payload: &'a [u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    alpha: bool,
    blend: bool,
}

fn validate_webp_preview<'a>(
    bytes: &'a [u8],
    cancel: &Cancellation,
) -> Result<Option<WebpPreviewFrame<'a>>> {
    fn invalid() -> ToolError {
        ToolError::Invalid("WebP 容器或首帧尺寸无效".into())
    }
    fn u24(bytes: &[u8]) -> u32 {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
    }
    fn chunks<'a>(
        mut bytes: &'a [u8],
        cancel: &Cancellation,
        mut visit: impl FnMut(&'a [u8], &'a [u8]) -> Result<()>,
    ) -> Result<()> {
        while !bytes.is_empty() {
            cancel.check()?;
            if bytes.len() < 8 {
                return Err(invalid());
            }
            let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            let end = 8usize.checked_add(length).ok_or_else(invalid)?;
            let padded = end.checked_add(length & 1).ok_or_else(invalid)?;
            if padded > bytes.len() {
                return Err(invalid());
            }
            visit(&bytes[..4], &bytes[8..end])?;
            bytes = &bytes[padded..];
        }
        Ok(())
    }
    fn bitstream(kind: &[u8], data: &[u8], expected: Option<(u32, u32)>) -> Result<bool> {
        let dimensions = if kind == b"VP8 " {
            if data.len() < 10 || data[0] & 1 != 0 || data[3..6] != [0x9d, 1, 0x2a] {
                return Err(invalid());
            }
            (
                u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3fff),
                u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3fff),
            )
        } else if kind == b"VP8L" {
            if data.len() < 5 || data[0] != 0x2f {
                return Err(invalid());
            }
            let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
            ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        } else {
            return Ok(false);
        };
        validate_dimensions(dimensions.0, dimensions.1)?;
        if expected.is_some_and(|expected| expected != dimensions) {
            return Err(invalid());
        }
        Ok(true)
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(invalid());
    }
    let end = 8usize
        .checked_add(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize)
        .ok_or_else(invalid)?;
    if end < 12 || end > bytes.len() {
        return Err(invalid());
    }
    let mut canvas = None;
    let mut first_frame = false;
    let mut frame = None;
    let mut animated = false;
    let mut animation_header = false;
    chunks(&bytes[12..end], cancel, |kind, data| {
        if kind == b"VP8X" {
            if data.len() != 10 || canvas.is_some() {
                return Err(invalid());
            }
            let dimensions = (u24(&data[4..7]) + 1, u24(&data[7..10]) + 1);
            validate_dimensions(dimensions.0, dimensions.1)?;
            canvas = Some(dimensions);
            animated = data[0] & 2 != 0;
        } else if kind == b"ANIM" {
            if !animated || animation_header || data.len() != 6 {
                return Err(invalid());
            }
            animation_header = true;
        } else if kind == b"ANMF" && !first_frame {
            if !animated || !animation_header {
                return Err(invalid());
            }
            if data.len() < 16 {
                return Err(invalid());
            }
            let (width, height) = canvas.ok_or_else(invalid)?;
            let (x, y) = (u24(&data[..3]) * 2, u24(&data[3..6]) * 2);
            let dimensions = (u24(&data[6..9]) + 1, u24(&data[9..12]) + 1);
            if x + dimensions.0 > width || y + dimensions.1 > height {
                return Err(invalid());
            }
            let mut found = false;
            let mut alpha = false;
            chunks(&data[16..], cancel, |kind, payload| {
                if kind == b"ALPH" {
                    if alpha || found {
                        return Err(invalid());
                    }
                    alpha = true;
                } else if bitstream(kind, payload, Some(dimensions))? {
                    if found || kind == b"VP8L" && alpha {
                        return Err(invalid());
                    }
                    if kind == b"VP8L" {
                        alpha = payload[4] & 0x10 != 0;
                    }
                    found = true;
                } else {
                    return Err(invalid());
                }
                Ok(())
            })?;
            if !found {
                return Err(invalid());
            }
            first_frame = true;
            frame = Some(WebpPreviewFrame {
                payload: &data[16..],
                x,
                y,
                width: dimensions.0,
                height: dimensions.1,
                alpha,
                blend: data[15] & 2 == 0,
            });
        } else if bitstream(kind, data, canvas)? {
            if animated || first_frame {
                return Err(invalid());
            }
            first_frame = true;
        }
        Ok(())
    })?;
    if !first_frame {
        return Err(invalid());
    }
    Ok(frame)
}

pub(crate) fn parse_icc(icc: &[u8]) -> Result<ColorProfile> {
    if icc.len() > MAX_ICC_BYTES {
        return Err(ToolError::Invalid("ICC 超过 4 MiB 安全限制".into()));
    }
    let profile = ColorProfile::new_from_slice(icc)
        .map_err(|e| ToolError::Invalid(format!("ICC 无效：{e}")))?;
    let is_gray = profile.color_space == DataColorSpace::Gray;
    if profile.color_space != DataColorSpace::Rgb && !is_gray {
        return Err(ToolError::Invalid(
            "当前不支持此 ICC 颜色空间（例如 CMYK）；颜色未核验".into(),
        ));
    }
    Ok(profile)
}

pub(crate) fn transform_icc(
    rgba: &mut RgbaImage,
    profile: &ColorProfile,
    gray: Option<&image::GrayAlphaImage>,
    cancel: &Cancellation,
) -> Result<()> {
    let dest = ColorProfile::new_srgb();
    let is_gray = profile.color_space == DataColorSpace::Gray;
    let layout = if is_gray {
        Layout::GrayAlpha
    } else {
        Layout::Rgba
    };
    let transform = profile
        .create_transform_8bit(layout, &dest, Layout::Rgba, TransformOptions::default())
        .map_err(|e| ToolError::Invalid(format!("ICC 转换不可用：{e}")))?;
    let source = if is_gray {
        gray.ok_or_else(|| ToolError::Invalid("缺少原始灰度 ICC 输入".into()))?
            .as_raw()
            .as_slice()
    } else {
        rgba.as_raw()
    };
    let stride = rgba.width() as usize * if is_gray { 2 } else { 4 };
    let out_stride = rgba.width() as usize * 4;
    let mut converted = RgbaImage::new(rgba.width(), rgba.height());
    for (src, dst) in source.chunks_exact(stride).zip(
        converted
            .as_flat_samples_mut()
            .samples
            .chunks_exact_mut(out_stride),
    ) {
        cancel.check()?;
        transform
            .transform(src, dst)
            .map_err(|e| ToolError::Invalid(format!("ICC 转换失败：{e}")))?;
    }
    *rgba = converted;
    Ok(())
}

pub const MAX_SIBLING_PATH_BYTES: usize = 4 * 1024 * 1024;

pub fn sibling_images(path: &Path) -> Vec<PathBuf> {
    sibling_images_cancellable(path, &Cancellation::default())
        .unwrap_or_else(|_| vec![path.to_path_buf()])
}

pub fn sibling_images_cancellable(path: &Path, cancel: &Cancellation) -> Result<Vec<PathBuf>> {
    cancel.check()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut path_bytes = path.as_os_str().len();
    if path_bytes > MAX_SIBLING_PATH_BYTES {
        return Err(ToolError::Invalid("浏览路径超出列表预算".into()));
    }
    let mut paths = vec![path.to_path_buf()];
    let entries = std::fs::read_dir(parent);
    cancel.check()?;
    let Ok(mut entries) = entries else {
        return Ok(paths);
    };
    let mut canonical_source = None;
    for _ in 0..10_000 {
        cancel.check()?;
        let Some(entry) = entries.next() else {
            break;
        };
        cancel.check()?;
        let Ok(entry) = entry else {
            continue;
        };
        let candidate = entry.path();
        let supported = candidate.extension().is_some_and(|e| {
            matches!(
                e.to_string_lossy().to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "gif" | "webp"
            )
        });
        if !supported {
            continue;
        }
        if candidate.file_name() == path.file_name() {
            continue;
        }
        if cfg!(windows)
            && candidate
                .file_name()
                .zip(path.file_name())
                .is_some_and(|(a, b)| {
                    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
                })
        {
            if canonical_source.is_none() {
                canonical_source = Some(path.canonicalize().ok());
            }
            cancel.check()?;
            let canonical_candidate = candidate.canonicalize();
            cancel.check()?;
            if canonical_candidate.ok().as_ref()
                == canonical_source.as_ref().and_then(|p| p.as_ref())
                && canonical_source.as_ref().is_some_and(|p| p.is_some())
            {
                continue;
            }
        }
        cancel.check()?;
        let is_file = candidate.is_file();
        cancel.check()?;
        if !is_file {
            continue;
        }
        let bytes = candidate.as_os_str().len();
        if paths.len() >= 10_000 || path_bytes.saturating_add(bytes) > MAX_SIBLING_PATH_BYTES {
            break;
        }
        path_bytes += bytes;
        paths.push(candidate);
    }
    cancel.check()?;
    paths.sort_by_key(|p| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    });
    cancel.check()?;
    Ok(paths)
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use image::{ImageEncoder, Rgba};
    use std::{borrow::Cow, io::Write};

    fn exif(orientation: u16) -> Vec<u8> {
        let mut bytes = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        bytes.extend_from_slice(&orientation.to_le_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }
    fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            bytes.push(0);
        }
        bytes
    }
    fn animated_webp(first: &[u8], second: &[u8]) -> Vec<u8> {
        let mut chunks = riff_chunk(b"VP8X", &[2, 0, 0, 0, 3, 0, 0, 1, 0, 0]);
        chunks.extend(riff_chunk(b"ANIM", &[0; 6]));
        for stream in [first, second] {
            let mut frame = vec![1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 10, 0, 0, 0];
            frame.extend_from_slice(stream);
            chunks.extend(riff_chunk(b"ANMF", &frame));
        }
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend(chunks);
        bytes
    }

    #[test]
    fn cancellation_reader_checks_read_buffer_and_seek_without_retryable_interrupts() {
        let bytes = vec![1; READ_CHECK_BYTES * 3];
        let cancel = Cancellation::default();
        let mut reader = CancellableReader::new(&bytes, &cancel);
        assert_eq!(reader.fill_buf().unwrap().len(), READ_CHECK_BYTES);
        let mut output = vec![0; bytes.len()];
        assert_eq!(reader.read(&mut output).unwrap(), READ_CHECK_BYTES);
        cancel.cancel();
        assert_eq!(
            reader.read(&mut output).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert!(reader.fill_buf().is_err());
        assert!(reader.seek(SeekFrom::Start(0)).is_err());
    }

    #[test]
    fn jpeg_preview_uses_scaled_idct_and_preserves_exif_and_icc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p3-oriented.jpg");
        let source = image::RgbImage::from_pixel(1024, 512, image::Rgb([180, 100, 80]));
        let mut encoded = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 95);
        encoder
            .set_icc_profile(ColorProfile::new_display_p3().encode().unwrap())
            .unwrap();
        encoder
            .write_image(source.as_raw(), 1024, 512, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(exif(6));
        let mut bytes = encoded[..2].to_vec();
        bytes.extend_from_slice(&[0xff, 0xe1]);
        bytes.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend(app1);
        bytes.extend_from_slice(&encoded[2..]);
        std::fs::write(&path, &bytes).unwrap();
        let cancel = Cancellation::default();
        {
            let mut reservation = crate::memory::reserve(MAX_DECODE_BYTES).unwrap();
            let preview = jpeg_preview(&bytes, 96, &cancel, &mut reservation).unwrap();
            assert_eq!((preview.image.width(), preview.image.height()), (128, 64));
            assert_eq!(preview.orientation, image::metadata::Orientation::Rotate90);
            assert!(preview.icc.is_some());
        }
        let thumb = thumbnail(&path, 96, &cancel).unwrap();
        assert_eq!(thumb.dimensions(), (48, 96));
        let full = load(&path, &cancel).unwrap();
        assert!(full.color_managed);
        for channel in 0..3 {
            assert!(
                thumb.get_pixel(0, 0)[channel].abs_diff(full.pixels.get_pixel(0, 0)[channel]) <= 3
            );
        }
        assert!(thumb.pixels().all(|pixel| pixel[3] == 255));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn png_preview_preserves_rgb_gray_icc_and_alpha_without_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        for gray in [false, true] {
            let path = dir.path().join(if gray { "gray.png" } else { "p3.png" });
            let mut bytes = Vec::new();
            let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
            let (data, color, icc) = if gray {
                (
                    [100, 128].repeat(64),
                    image::ExtendedColorType::La8,
                    ColorProfile::new_gray_with_gamma(1.8).encode().unwrap(),
                )
            } else {
                (
                    [180, 100, 80, 128].repeat(64),
                    image::ExtendedColorType::Rgba8,
                    ColorProfile::new_display_p3().encode().unwrap(),
                )
            };
            encoder.set_icc_profile(icc).unwrap();
            encoder.write_image(&data, 8, 8, color).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let before = crate::memory::in_use();
            let thumb = thumbnail(&path, 96, &cancel).unwrap();
            assert_eq!(crate::memory::in_use(), before);
            let full = load(&path, &cancel).unwrap();
            assert!(full.color_managed);
            assert_eq!(thumb, **full.pixels);
            assert!(thumb.pixels().all(|pixel| pixel[3] == 128));
        }
    }

    #[test]
    fn png_row_decoder_preserves_sixteen_bit_samples_and_adam7_pixels() {
        let cancel = Cancellation::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sixteen.png");
        let original = image::ImageBuffer::<Rgba<u16>, Vec<u16>>::from_fn(7, 5, |x, y| {
            Rgba([x as u16 * 1000, y as u16 * 2000, 32768, x as u16 * 5000])
        });
        original.save(&path).unwrap();
        let decoded = png_pixels(&std::fs::read(path).unwrap(), &cancel).unwrap();
        assert_eq!(decoded, DynamicImage::ImageRgba16(original));

        let pixels = RgbaImage::from_fn(8, 8, |x, y| {
            Rgba([x as u8 * 31, y as u8 * 31, 17, (x + y) as u8 * 17])
        });
        let mut rows = Vec::new();
        for (start_x, start_y, step_x, step_y) in [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ] {
            for y in (start_y..8).step_by(step_y) {
                rows.push(0);
                for x in (start_x..8).step_by(step_x) {
                    rows.extend_from_slice(&pixels.get_pixel(x, y).0);
                }
            }
        }
        fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = bytes.len();
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(&crc32fast::hash(&bytes[start..]).to_be_bytes());
        }
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(&rows).unwrap();
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = 8u32.to_be_bytes().repeat(2);
        ihdr.extend_from_slice(&[8, 6, 0, 0, 1]);
        chunk(&mut bytes, b"IHDR", &ihdr);
        chunk(&mut bytes, b"IDAT", &compressed.finish().unwrap());
        chunk(&mut bytes, b"IEND", &[]);
        assert_eq!(
            png_pixels(&bytes, &cancel).unwrap(),
            DynamicImage::ImageRgba8(pixels)
        );
    }

    #[test]
    fn gif_preview_uses_first_local_frame_on_transparent_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.gif");
        let mut bytes = Vec::new();
        {
            let mut encoder =
                gif::Encoder::new(&mut bytes, 8, 4, &[0, 0, 0, 180, 100, 80]).unwrap();
            let frame = gif::Frame {
                left: 2,
                top: 1,
                width: 2,
                height: 2,
                buffer: Cow::Owned(vec![1; 4]),
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
            encoder
                .write_frame(&gif::Frame {
                    width: 8,
                    height: 4,
                    buffer: Cow::Owned(vec![0; 32]),
                    ..Default::default()
                })
                .unwrap();
        }
        std::fs::write(&path, &bytes).unwrap();
        let thumb = thumbnail(&path, 96, &Cancellation::default()).unwrap();
        assert_eq!(thumb.dimensions(), (8, 4));
        assert_eq!(thumb.get_pixel(0, 0)[3], 0);
        assert_eq!(thumb.get_pixel(2, 1).0, [180, 100, 80, 255]);
    }

    #[test]
    fn webp_preview_does_not_decode_later_frames_and_preserves_transparent_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.webp");
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .write_image(
                &[180, 100, 80, 180, 100, 80],
                2,
                1,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        let bad_second = riff_chunk(b"VP8L", &[0x2f, 1, 0, 0, 0, 0]);
        let bytes = animated_webp(&encoded[12..], &bad_second);
        std::fs::write(&path, &bytes).unwrap();
        let thumb = thumbnail(&path, 96, &Cancellation::default()).unwrap();
        assert_eq!(thumb.dimensions(), (4, 2));
        assert_eq!(thumb.get_pixel(0, 0)[3], 0);
        assert_eq!(thumb.get_pixel(2, 0).0, [180, 100, 80, 255]);
        assert_eq!(thumb.get_pixel(3, 1)[3], 0);

        let oversized = riff_chunk(b"VP8L", &[0x2f, 0x9f, 0x0f, 0xe8, 0x03]);
        assert!(
            validate_webp_preview(
                &animated_webp(&oversized, &bad_second),
                &Cancellation::default()
            )
            .is_err()
        );
    }

    #[test]
    fn preview_cancellation_and_input_bounds_are_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.png");
        RgbaImage::new(2, 2).save(&path).unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            thumbnail(&path, 96, &cancel),
            Err(ToolError::Cancelled)
        ));
        assert!(matches!(load(&path, &cancel), Err(ToolError::Cancelled)));
        assert!(thumbnail(&path, 257, &Cancellation::default()).is_err());
    }
}
