//! Narrow wire-format validation. Never a general BMP parser.
use super::*;
use image::{DynamicImage, ImageEncoder};
use std::io::{Cursor, Write};

pub(super) fn rgba_len(width: u32, height: u32) -> ClipboardResult<usize> {
    if width == 0
        || height == 0
        || width > crate::pipeline::MAX_DIMENSION
        || height > crate::pipeline::MAX_DIMENSION
        || u64::from(width) * u64::from(height) > crate::pipeline::MAX_PIXELS
    {
        return Err(ClipboardError::TooLarge("最多 1600 万像素，单边最多 32768"));
    }
    usize::try_from(u64::from(width) * u64::from(height) * 4)
        .map_err(|_| ClipboardError::TooLarge("像素长度溢出"))
}

fn u16_at(bytes: &[u8], offset: usize) -> ClipboardResult<u16> {
    let field = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("字段被截断"))?;
    Ok(u16::from_le_bytes([field[0], field[1]]))
}
fn u32_at(bytes: &[u8], offset: usize) -> ClipboardResult<u32> {
    let field = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("字段被截断"))?;
    Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}
fn invalid(message: &str) -> ClipboardError {
    ClipboardError::Invalid(message.into())
}

pub(super) fn zeroed(length: usize) -> ClipboardResult<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(length)
        .map_err(|_| ClipboardError::Resource("缓冲区分配失败".into()))?;
    out.resize(length, 0);
    Ok(out)
}

/// Fixed IHDR preflight, performed before a clipboard block is cloned or decoded.
pub(super) fn png_dimensions(bytes: &[u8]) -> ClipboardResult<(u32, u32)> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(ClipboardError::TooLarge("编码图像超过 64 MiB"));
    }
    if bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || bytes[8..12] != 13u32.to_be_bytes()
        || &bytes[12..16] != b"IHDR"
    {
        return Err(invalid("PNG 签名或 IHDR 不完整"));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    rgba_len(width, height)?;
    Ok((width, height))
}

pub(super) fn decode_png(bytes: &[u8]) -> ClipboardResult<ClipboardImage> {
    let (width, height) = png_dimensions(bytes)?;
    let rgba_size = rgba_len(width, height)?;
    // Input is owned/accounted by the snapshot. Bound codec scratch separately,
    // plus raw RGBA, conversion, orientation and grayscale working images.
    let mut reservation = crate::memory::reserve(
        crate::pipeline::MAX_DECODE_BYTES + rgba_size as u64 * 5 + crate::memory::OVERHEAD,
    )
    .map_err(admission)?;
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: crate::pipeline::MAX_DECODE_BYTES as usize,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| invalid(&format!("PNG：{e}")))?;
    if reader.info().animation_control.is_some() {
        return Err(ClipboardError::UnsupportedData("动画 PNG（APNG）"));
    }
    if (reader.info().width, reader.info().height) != (width, height) {
        return Err(invalid("PNG 尺寸不一致"));
    }
    let was_16_bit = reader.info().bit_depth == png::BitDepth::Sixteen;
    let orientation = reader
        .info()
        .exif_metadata
        .as_deref()
        .and_then(image::metadata::Orientation::from_exif_chunk)
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // Borrow metadata instead of image::ImageDecoder::icc_profile(), which clones
    // the whole profile before the application can check its length.
    let prepared_icc = reader
        .info()
        .icc_profile
        .as_deref()
        .map(crate::pipeline::parse_icc);
    let size = reader
        .output_buffer_size()
        .ok_or(ClipboardError::TooLarge("PNG 解码长度溢出"))?;
    if size > rgba_size {
        return Err(ClipboardError::TooLarge("PNG 输出超过预检像素预算"));
    }
    let mut raw = zeroed(size)?;
    let info = reader
        .next_frame(&mut raw)
        .map_err(|e| invalid(&format!("PNG：{e}")))?;
    reader
        .finish()
        .map_err(|e| invalid(&format!("PNG 结束块：{e}")))?;
    if info.width != width || info.height != height || info.bit_depth != png::BitDepth::Eight {
        return Err(invalid("PNG 输出布局与预检不一致"));
    }
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(invalid("PNG 调色板未展开")),
    };
    let count = rgba_size / 4;
    if info.buffer_size() != count * channels || raw.len() != count * channels {
        return Err(invalid("PNG 像素缓冲长度不一致"));
    }
    let mut rgba = zeroed(rgba_size)?;
    for (source, target) in raw
        .chunks_exact(channels)
        .zip(rgba.as_chunks_mut::<4>().0.iter_mut())
    {
        let pixel = match channels {
            1 => [source[0], source[0], source[0], 255],
            2 => [source[0], source[0], source[0], source[1]],
            3 => [source[0], source[1], source[2], 255],
            4 => [source[0], source[1], source[2], source[3]],
            _ => unreachable!(),
        };
        target.copy_from_slice(&pixel);
    }
    drop(raw);
    drop(reader);
    let mut rgba =
        RgbaImage::from_raw(width, height, rgba).ok_or_else(|| invalid("PNG 像素长度无效"))?;
    let mut warnings = vec!["剪贴板临时图像；没有可核验的原文件来源或原文件签名".into()];
    if was_16_bit {
        warnings.push("剪贴板显示与导出副本转换为 8 位".into());
    }
    let mut color_managed = true;
    let color_status = if let Some(profile) = prepared_icc {
        let converted = profile.and_then(|profile| {
            let gray = (profile.color_space == moxcms::DataColorSpace::Gray)
                .then(|| DynamicImage::ImageRgba8(rgba.clone()).to_luma_alpha8());
            crate::pipeline::transform_icc(
                &mut rgba,
                &profile,
                gray.as_ref(),
                &crate::Cancellation::default(),
            )
        });
        match converted {
            Ok(()) => "剪贴板 PNG ICC 已转换为 sRGB".into(),
            Err(error) => {
                color_managed = false;
                warnings.push(error.to_string());
                "剪贴板 PNG ICC 未完整处理；颜色可能不准确".into()
            }
        }
    } else {
        "剪贴板颜色信息未知，按 sRGB 显示".into()
    };
    let mut oriented = DynamicImage::ImageRgba8(rgba);
    oriented.apply_orientation(orientation);
    let pixels = Arc::new(
        crate::memory::Pixels::from_reserved(oriented.into_rgba8(), &mut reservation)
            .map_err(admission)?,
    );
    Ok(ClipboardImage {
        pixels,
        source: ClipboardSource::Png,
        color_status,
        color_managed,
        warnings,
    })
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DibLayout {
    pub width: u32,
    pub height: u32,
    pub top_down: bool,
    pub stride: usize,
    pub pixel_offset: usize,
    pub bytes_per_pixel: usize,
    pub alpha: bool,
    pub srgb: bool,
}

pub(super) fn dib_layout(bytes: &[u8], source: ClipboardSource) -> ClipboardResult<DibLayout> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(ClipboardError::TooLarge("DIB 超过 64 MiB"));
    }
    let header = u32_at(bytes, 0)? as usize;
    if !matches!(header, 40 | 124) || (source == ClipboardSource::DibV5 && header != 124) {
        return Err(ClipboardError::UnsupportedData(
            "仅支持 40 字节 DIB 或 124 字节 DIBV5 头",
        ));
    }
    if bytes.len() < header {
        return Err(invalid("DIB 头被截断"));
    }
    let width = u32_at(bytes, 4)? as i32;
    let signed_height = u32_at(bytes, 8)? as i32;
    if width <= 0 || signed_height == 0 {
        return Err(invalid("DIB 尺寸无效"));
    }
    let height = signed_height
        .checked_abs()
        .ok_or(ClipboardError::TooLarge("DIB 高度溢出"))? as u32;
    rgba_len(width as u32, height)?;
    if u16_at(bytes, 12)? != 1 {
        return Err(invalid("DIB 平面数必须为 1"));
    }
    let bits = u16_at(bytes, 14)?;
    let compression = u32_at(bytes, 16)?;
    if !matches!((bits, compression), (24 | 32, 0) | (32, 3)) {
        return Err(ClipboardError::UnsupportedData(
            "仅支持未压缩 24/32 位或标准 32 位位域 DIB",
        ));
    }
    if u32_at(bytes, 32)? != 0 {
        return Err(ClipboardError::UnsupportedData("带调色板的 DIB"));
    }
    let mut pixel_offset = header;
    let mut alpha = false;
    // Narrow real-world compatibility also used by pinned arboard 3.6.1:
    // Chromium/Electron may advertise 32-bit BI_RGB DIBV5 with an explicit
    // full alpha mask, and standard or zero RGB masks. Preserve that alpha.
    // Ordinary BI_RGB without that explicit V5 mask remains opaque.
    if header == 124 && bits == 32 && compression == 0 {
        let masks = (u32_at(bytes, 40)?, u32_at(bytes, 44)?, u32_at(bytes, 48)?);
        match u32_at(bytes, 52)? {
            0 => {}
            0xff000000 if matches!(masks, (0, 0, 0) | (0x00ff0000, 0x0000ff00, 0x000000ff)) => {
                alpha = true
            }
            _ => {
                return Err(ClipboardError::UnsupportedData(
                    "非标准 BI_RGB DIBV5 alpha 位域",
                ));
            }
        }
    }
    if compression == 3 {
        if u32_at(bytes, 40)? != 0x00ff0000
            || u32_at(bytes, 44)? != 0x0000ff00
            || u32_at(bytes, 48)? != 0x000000ff
        {
            return Err(ClipboardError::UnsupportedData("非标准 RGB 位域"));
        }
        if header == 40 {
            pixel_offset += 12;
        } else {
            alpha = match u32_at(bytes, 52)? {
                0 => false,
                0xff000000 => true,
                _ => return Err(ClipboardError::UnsupportedData("非标准 alpha 位域")),
            };
        }
    }
    let mut srgb = false;
    if header == 124 {
        // Never dereference linked color profiles or resolve filenames from pixels.
        let space = u32_at(bytes, 56)?;
        srgb = matches!(space, 0x73524742 | 0x57696e20); // LCS_sRGB / LCS_WINDOWS_COLOR_SPACE
        if (!srgb && space != 0) || u32_at(bytes, 112)? != 0 || u32_at(bytes, 116)? != 0 {
            return Err(ClipboardError::UnsupportedData(
                "带链接、嵌入或非 sRGB 色彩空间的 DIBV5；请复制 PNG",
            ));
        }
        // LCS_CALIBRATED_RGB with supplied endpoints/gamma is not unknown/sRGB.
        if space == 0 && bytes[60..108].iter().any(|value| *value != 0) {
            return Err(ClipboardError::UnsupportedData(
                "校准 RGB DIBV5；请复制 PNG",
            ));
        }
        if u32_at(bytes, 120)? != 0 {
            return Err(invalid("DIBV5 保留字段非零"));
        }
    }
    let row_bits = u64::from(width as u32)
        .checked_mul(u64::from(bits))
        .ok_or(ClipboardError::TooLarge("DIB 行溢出"))?;
    let stride = row_bits
        .checked_add(31)
        .and_then(|n| (n / 32).checked_mul(4))
        .ok_or(ClipboardError::TooLarge("DIB stride 溢出"))?;
    let image_bytes = stride
        .checked_mul(u64::from(height))
        .ok_or(ClipboardError::TooLarge("DIB 长度溢出"))?;
    let end = (pixel_offset as u64)
        .checked_add(image_bytes)
        .ok_or(ClipboardError::TooLarge("DIB 偏移溢出"))?;
    if end > bytes.len() as u64 {
        return Err(invalid("DIB 像素数据被截断"));
    }
    let declared = u64::from(u32_at(bytes, 20)?);
    if declared != 0 && (declared < image_bytes || declared > (bytes.len() - pixel_offset) as u64) {
        return Err(invalid("DIB 声明的图像长度不一致"));
    }
    Ok(DibLayout {
        width: width as u32,
        height,
        top_down: signed_height < 0,
        stride: stride as usize,
        pixel_offset,
        bytes_per_pixel: bits as usize / 8,
        alpha,
        srgb,
    })
}

pub(super) fn decode_dib(bytes: &[u8], source: ClipboardSource) -> ClipboardResult<ClipboardImage> {
    let layout = dib_layout(bytes, source)?;
    let size = rgba_len(layout.width, layout.height)?;
    let mut reservation = crate::memory::reserve(size as u64).map_err(admission)?;
    let mut rgba = zeroed(size)?;
    for y in 0..layout.height as usize {
        let source_y = if layout.top_down {
            y
        } else {
            layout.height as usize - 1 - y
        };
        let start = layout.pixel_offset + source_y * layout.stride;
        let row = &bytes[start..start + layout.width as usize * layout.bytes_per_pixel];
        let target = &mut rgba[y * layout.width as usize * 4..(y + 1) * layout.width as usize * 4];
        for (input, output) in row
            .chunks_exact(layout.bytes_per_pixel)
            .zip(target.as_chunks_mut::<4>().0.iter_mut())
        {
            output.copy_from_slice(&[
                input[2],
                input[1],
                input[0],
                if layout.alpha { input[3] } else { 255 },
            ]);
        }
    }
    let pixels = RgbaImage::from_raw(layout.width, layout.height, rgba)
        .ok_or_else(|| invalid("DIB 像素长度无效"))?;
    let mut warnings =
        vec!["剪贴板临时图像；没有原文件字节、EXIF、ICC 或可核验的原文件签名".into()];
    if !layout.alpha {
        warnings.push("此 DIB 格式没有定义 alpha，按不透明像素读取；需要透明度时请复制 PNG 或带 alpha 的 DIBV5".into());
    }
    Ok(ClipboardImage {
        pixels: Arc::new(
            crate::memory::Pixels::from_reserved(pixels, &mut reservation).map_err(admission)?,
        ),
        source,
        color_status: if layout.srgb {
            "剪贴板 DIBV5 标记为 sRGB"
        } else {
            "剪贴板颜色信息未知，按 sRGB 显示"
        }
        .into(),
        color_managed: true,
        warnings,
    })
}

pub(super) fn encode_dibv5(pixels: &RgbaImage) -> ClipboardResult<Vec<u8>> {
    let size = rgba_len(pixels.width(), pixels.height())?;
    if pixels.as_raw().len() != size {
        return Err(invalid("RGBA 像素缓冲长度无效"));
    }
    let mut output = zeroed(size + 124)?;
    let mut put = |offset: usize, value: u32| {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
    };
    put(0, 124);
    put(4, pixels.width());
    put(8, pixels.height());
    put(12, 1 | (32 << 16));
    put(16, 3);
    put(20, size as u32);
    put(40, 0x00ff0000);
    put(44, 0x0000ff00);
    put(48, 0x000000ff);
    put(52, 0xff000000);
    put(56, 0x73524742);
    put(108, 4); // sRGB; LCS_GM_IMAGES
    let stride = pixels.width() as usize * 4;
    for (input, output_row) in pixels
        .as_raw()
        .chunks_exact(stride)
        .rev()
        .zip(output[124..].chunks_exact_mut(stride))
    {
        for (rgba, bgra) in input
            .as_chunks::<4>()
            .0
            .iter()
            .zip(output_row.as_chunks_mut::<4>().0.iter_mut())
        {
            bgra.copy_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
        }
    }
    Ok(output)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| std::io::Error::other("PNG 编码超过 64 MiB"))?;
        if needed > self.bytes.capacity() {
            let desired = needed.saturating_add(1024 * 1024).min(self.limit);
            self.bytes
                .try_reserve_exact(desired - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode_png(pixels: &RgbaImage) -> ClipboardResult<Vec<u8>> {
    let size = rgba_len(pixels.width(), pixels.height())?;
    if pixels.as_raw().len() != size {
        return Err(invalid("RGBA 像素缓冲长度无效"));
    }
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        limit: MAX_CLIPBOARD_BYTES,
    };
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| invalid(&format!("PNG 编码失败：{e}")))?;
    Ok(output.bytes)
}

/// Validates only unambiguous drive-absolute Windows image paths. No I/O here.
pub(super) fn local_path(text: &str) -> ClipboardResult<PathBuf> {
    if text.encode_utf16().count() > MAX_PATH_UNITS {
        return Err(ClipboardError::TooLarge("路径超过 32767 个 UTF-16 字符"));
    }
    let text = text.trim();
    let text = if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
        &text[1..text.len() - 1]
    } else {
        text
    };
    let bytes = text.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
        || text
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | '*' | '?'))
        || text[2..].contains(':')
    {
        return Err(ClipboardError::NonLocalPath);
    }
    let components: Vec<_> = text[3..].split(['/', '\\']).collect();
    if components
        .iter()
        .any(|part| part.is_empty() || matches!(*part, "." | "..") || part.ends_with([' ', '.']))
    {
        return Err(ClipboardError::NonLocalPath);
    }
    for part in &components {
        let stem = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        }) {
            return Err(ClipboardError::NonLocalPath);
        }
    }
    let ext = components
        .last()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if !matches!(
        ext.as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "ico")
    ) {
        return Err(ClipboardError::UnsupportedData(
            "路径必须明确指向 JPEG、PNG、GIF、WebP、BMP 或 ICO 图片",
        ));
    }
    Ok(PathBuf::from(text.replace('/', "\\")))
}

pub(super) fn decode_unicode_text(bytes: &[u8]) -> ClipboardResult<String> {
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(ClipboardError::TooLarge("文本路径超过长度限制"));
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(invalid("UTF-16 数据长度不是偶数"));
    }
    let count = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or_else(|| invalid("UTF-16 缺少结束符"))?;
    if count > MAX_PATH_UNITS {
        return Err(ClipboardError::TooLarge("文本路径超过长度限制"));
    }
    let units = bytes[..count * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]));
    std::char::decode_utf16(units)
        .map(|ch| ch.map_err(|_| invalid("无效 UTF-16 路径")))
        .collect()
}

pub(super) fn encode_text(text: &str) -> ClipboardResult<Vec<u8>> {
    if text.contains('\0') {
        return Err(invalid("文本含有 NUL"));
    }
    let count = text.encode_utf16().count();
    if count > MAX_PATH_UNITS {
        return Err(ClipboardError::TooLarge("文本超过长度限制"));
    }
    let mut bytes = zeroed((count + 1) * 2)?;
    for (unit, target) in text
        .encode_utf16()
        .zip(bytes.as_chunks_mut::<2>().0.iter_mut())
    {
        target.copy_from_slice(&unit.to_le_bytes());
    }
    Ok(bytes)
}

/// Strict Unicode DROPFILES only. A malformed list never falls through to text.
pub(super) fn decode_hdrop(bytes: &[u8]) -> ClipboardResult<PathBuf> {
    if bytes.len() > MAX_TEXT_BYTES + 22 {
        return Err(ClipboardError::TooLarge("文件引用超过长度限制"));
    }
    if bytes.len() < 24 {
        return Err(invalid("DROPFILES 被截断"));
    }
    let offset = u32_at(bytes, 0)? as usize;
    if offset < 20 || offset > bytes.len() - 4 || !offset.is_multiple_of(2) {
        return Err(invalid("DROPFILES 路径偏移无效"));
    }
    if u32_at(bytes, 16)? != 1 {
        return Err(ClipboardError::UnsupportedData("旧式非 Unicode 文件引用"));
    }
    let payload = &bytes[offset..];
    if !payload.len().is_multiple_of(2) {
        return Err(invalid("DROPFILES UTF-16 长度无效"));
    }
    let end = payload
        .as_chunks::<2>()
        .0
        .iter()
        .position(|unit| *unit == [0, 0])
        .ok_or_else(|| invalid("DROPFILES 缺少结束符"))?;
    if end == 0 {
        return Err(ClipboardError::NoSupportedFormat);
    }
    let after = (end + 1) * 2;
    if payload.get(after..after + 2) != Some(&[0, 0]) {
        return Err(if payload.len() >= after + 2 {
            ClipboardError::MultipleFiles
        } else {
            invalid("DROPFILES 缺少双结束符")
        });
    }
    let text = decode_unicode_text(&payload[..after])?;
    local_path(&text)
}
