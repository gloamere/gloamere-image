use crate::{Cancellation, Result, ToolError};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 16_000_000;
pub const MAX_DIMENSION: u32 = 32768;
pub const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ICC_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct LoadedImage {
    pub animation: Option<Arc<crate::animation::AnimationSource>>,
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
    let started = Instant::now();
    let bytes = Arc::new(read_snapshot(path, cancel)?);
    let format = image::guess_format(&bytes)?;
    if let Some(animated) =
        crate::animation::try_load(bytes.clone(), path, format, cancel, started)?
    {
        return Ok(animated);
    }
    decode(&bytes, path, cancel, started)
}

pub fn read_snapshot(path: &Path, cancel: &Cancellation) -> Result<crate::memory::Snapshot> {
    cancel.check()?;
    let mut file = File::open(path)?;
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
    cancel: &Cancellation,
    started: Instant,
) -> Result<LoadedImage> {
    let mut reservation = crate::memory::reserve(MAX_DECODE_BYTES)?;
    // Never trust the extension. Restrict the accepted content formats explicitly.
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format().ok_or_else(|| {
        ToolError::Invalid("无法识别图片；当前支持 JPEG、PNG、GIF 和 WebP".into())
    })?;
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(ToolError::Invalid(
            "当前版本仅支持 JPEG、PNG、GIF 和 WebP".into(),
        ));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    if format == ImageFormat::Png
        && image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits.clone())?
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
    reservation.resize(crate::memory::decode_estimate(
        0,
        decoder.total_bytes(),
        w,
        h,
    ))?;
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
    let mut decoded = DynamicImage::from_decoder(decoder)?;
    cancel.check()?;
    decoded.apply_orientation(orientation);
    if source_type.bits_per_pixel() > u16::from(source_type.channel_count()) * 8 {
        warnings.push("显示与导出副本为 8 位；原始高位深文件保持不变".into());
    }
    let prepared_icc = icc.as_deref().map(parse_icc);
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
    let has_alpha = rgba.pixels().any(|p| p[3] < 255);
    cancel.check()?;
    let color_managed = !color_status.contains("未完整") && !color_status.contains("核验不完整");
    Ok(LoadedImage {
        animation: None,
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
        source_sha256: format!("{:x}", Sha256::digest(bytes)),
        warnings,
        load_ms: started.elapsed().as_millis(),
    })
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
