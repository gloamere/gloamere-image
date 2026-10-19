//! Bounded non-authenticated generation metadata. Nothing here authenticates pixels.
use super::types::{Evidence, EvidenceKind};
use crate::{Cancellation, Result, ToolError};
use std::io::{Cursor, Read};

const MAX_TEXT: usize = 64 * 1024;
const MAX_FIELDS: usize = 32;
const MAX_CHUNKS: usize = 8192;
fn generator(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "stable diffusion",
        "automatic1111",
        "comfyui",
        "midjourney",
        "dall-e",
        "novelai",
        "invokeai",
    ]
    .iter()
    .any(|name| value.contains(name))
}
fn generation_field(key: &str, value: &str) -> bool {
    match key.to_ascii_lowercase().as_str() {
        "software" | "exif software" | "creator" | "source" => generator(value),
        "parameters" => {
            value.contains("Steps:")
                && value.contains("Seed:")
                && (value.contains("Sampler:") || value.contains("Schedule type:"))
        }
        "prompt" | "workflow" => {
            value.contains("class_type")
                && ["KSampler", "SamplerCustom", "UNETLoader"]
                    .iter()
                    .any(|s| value.contains(s))
        }
        _ => false,
    }
}
fn push(fields: &mut Vec<Evidence>, key: &str, value: &str) {
    if fields.len() < MAX_FIELDS && generation_field(key, value) {
        fields.push(Evidence {
            kind: EvidenceKind::UnsignedGeneration,
            provider: format!("生成元数据 · {}", key.chars().take(64).collect::<String>()),
            // Prompts stay local; only a bounded summary is persisted.
            summary: value
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(512)
                .collect(),
            authenticated: false,
        });
    }
}
fn text(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_TEXT {
        return Err(ToolError::Invalid("生成元数据超过 64 KiB 限制".into()));
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}
fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .take(MAX_TEXT as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > MAX_TEXT {
        return Err(ToolError::Invalid("压缩元数据展开超过 64 KiB 限制".into()));
    }
    Ok(output)
}
fn png(bytes: &[u8], cancel: &Cancellation) -> Result<Vec<Evidence>> {
    let mut offset = 8usize;
    let mut fields = Vec::new();
    let mut field_count = 0;
    for _ in 0..MAX_CHUNKS {
        cancel.check()?;
        if offset.checked_add(12).is_none_or(|end| end > bytes.len()) {
            return Err(ToolError::Invalid("PNG 数据块不完整".into()));
        }
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(len))
            .filter(|n| *n <= bytes.len())
            .ok_or_else(|| ToolError::Invalid("PNG 数据块长度无效".into()))?;
        let kind = &bytes[offset + 4..offset + 8];
        let data = &bytes[offset + 8..offset + 8 + len];
        if kind == b"IHDR"
            || kind == b"IEND"
            || [b"tEXt", b"zTXt", b"iTXt"].contains(&kind.try_into().unwrap())
        {
            let expected = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
            let actual = crc32fast::hash(&bytes[offset + 4..end - 4]);
            if actual != expected {
                return Err(ToolError::Invalid("PNG 元数据 CRC 校验失败".into()));
            }
        }
        if kind == b"IEND" {
            return Ok(fields);
        }
        if [b"tEXt", b"zTXt", b"iTXt"].contains(&kind.try_into().unwrap()) {
            field_count += 1;
            if field_count > MAX_FIELDS {
                return Err(ToolError::Invalid("PNG 文本字段过多".into()));
            }
            let split = data
                .iter()
                .position(|b| *b == 0)
                .filter(|p| *p > 0 && *p <= 79)
                .ok_or_else(|| ToolError::Invalid("PNG 文本关键词无效".into()))?;
            let key = text(&data[..split])?;
            let body = &data[split + 1..];
            let value = if kind == b"tEXt" {
                text(body)?
            } else if kind == b"zTXt" {
                if body.first() != Some(&0) {
                    return Err(ToolError::Invalid("PNG 文本压缩方法无效".into()));
                }
                text(&inflate(&body[1..])?)?
            } else {
                if body.len() < 2 || body[0] > 1 || body[1] != 0 {
                    return Err(ToolError::Invalid("PNG 国际文本头无效".into()));
                }
                let mut rest = &body[2..];
                for _ in 0..2 {
                    let n = rest
                        .iter()
                        .position(|b| *b == 0)
                        .ok_or_else(|| ToolError::Invalid("PNG 国际文本字段不完整".into()))?;
                    rest = &rest[n + 1..];
                }
                if body[0] == 1 {
                    text(&inflate(rest)?)?
                } else {
                    text(rest)?
                }
            };
            push(&mut fields, &key, &value);
        }
        offset = end;
    }
    Err(ToolError::Invalid("PNG 数据块超过检查限制".into()))
}

fn webp_metadata_bound(bytes: &[u8], cancel: &Cancellation) -> Result<()> {
    if bytes.len() < 12 {
        return Err(ToolError::Invalid("WebP 头不完整".into()));
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8;
    if declared > bytes.len() {
        return Err(ToolError::Invalid("WebP 长度无效".into()));
    }
    let mut offset = 12usize;
    for _ in 0..MAX_CHUNKS {
        cancel.check()?;
        if offset == declared {
            return Ok(());
        }
        if offset.checked_add(8).is_none_or(|n| n > declared) {
            return Err(ToolError::Invalid("WebP 块不完整".into()));
        }
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if &bytes[offset..offset + 4] == b"EXIF" && size > MAX_TEXT {
            return Err(ToolError::Invalid("WebP EXIF 超过 64 KiB 限制".into()));
        }
        offset = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(size))
            .and_then(|n| n.checked_add(size % 2))
            .filter(|n| *n <= declared)
            .ok_or_else(|| ToolError::Invalid("WebP 块长度无效".into()))?;
    }
    Err(ToolError::Invalid("WebP 块超过检查限制".into()))
}
pub fn inspect(bytes: &[u8], cancel: &Cancellation) -> Result<Vec<Evidence>> {
    cancel.check()?;
    let format = image::guess_format(bytes)?;
    let mut fields = if format == image::ImageFormat::Png {
        png(bytes, cancel)?
    } else {
        Vec::new()
    };
    if format == image::ImageFormat::WebP {
        webp_metadata_bound(bytes, cancel)?;
    }
    if matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::WebP) {
        // EXIF parser sees only the bounded file snapshot, no network resolution.
        match exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
            Ok(exif) => {
                if let Some(field) = exif.get_field(exif::Tag::Software, exif::In::PRIMARY) {
                    let exif::Value::Ascii(parts) = &field.value else {
                        return Err(ToolError::Invalid("EXIF Software 字段类型无效".into()));
                    };
                    if parts.len() > 32 || parts.iter().map(Vec::len).sum::<usize>() > MAX_TEXT {
                        return Err(ToolError::Invalid("EXIF Software 超过文本限制".into()));
                    }
                    let value = parts
                        .iter()
                        .map(|part| String::from_utf8_lossy(part))
                        .collect::<Vec<_>>()
                        .join(" ");
                    push(&mut fields, "EXIF Software", &value);
                }
            }
            Err(exif::Error::NotFound(_)) => {}
            Err(error) => return Err(ToolError::Invalid(format!("EXIF 解析失败：{error}"))),
        }
    }
    cancel.check()?;
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png_text(key: &str, value: &str) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut out, 1, 1);
            encoder.add_text_chunk(key.into(), value.into()).unwrap();
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0])
                .unwrap();
        }
        out
    }
    #[test]
    fn arbitrary_prompt_is_not_generation_evidence() {
        for (key, value) in [
            ("prompt", "a photo"),
            ("software", "Photoshop"),
            ("parameters", "exposure=1"),
            ("description", "generated by AI"),
        ] {
            assert!(
                inspect(&png_text(key, value), &Cancellation::default())
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn supported_metadata_is_only_unsigned() {
        let r = inspect(
            &png_text("parameters", "cat\nSteps: 20, Sampler: Euler, Seed: 1"),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(r.len(), 1);
        assert!(!r[0].authenticated);
        assert_eq!(r[0].kind, EvidenceKind::UnsignedGeneration);
    }
    #[test]
    fn large_text_is_terminal_error() {
        assert!(
            inspect(
                &png_text("prompt", &"a".repeat(MAX_TEXT + 1)),
                &Cancellation::default()
            )
            .is_err()
        );
    }
    #[test]
    fn cancelled_read_and_truncated_file_are_errors() {
        let c = Cancellation::default();
        c.cancel();
        assert!(inspect(&png_text("software", "ComfyUI"), &c).is_err());
        let mut bytes = png_text("software", "ComfyUI");
        bytes.truncate(bytes.len() - 4);
        assert!(inspect(&bytes, &Cancellation::default()).is_err());
    }
}
