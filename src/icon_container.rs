//! Bounded ICO directory inspection. Pixel and AND-mask decoding stays in `image`.
use crate::{Cancellation, Result, ToolError};
use std::ops::Range;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
// A normal Windows icon has a handful of sizes. Bound metadata independently of file bytes.
pub const MAX_ICON_ENTRIES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconEncoding {
    Png,
    Dib,
}

impl IconEncoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Dib => "DIB",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconEntry {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u16,
    pub encoding: IconEncoding,
    range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconDirectory {
    pub entries: Vec<IconEntry>,
    pub selected_index: usize,
}

fn invalid(detail: &str) -> ToolError {
    ToolError::Invalid(format!("ICO 目录无效：{detail}"))
}

fn u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

impl IconDirectory {
    pub fn parse(bytes: &[u8], cancel: &Cancellation) -> Result<Self> {
        cancel.check()?;
        if bytes.len() < 6 || bytes[..4] != [0, 0, 1, 0] {
            return Err(invalid("仅支持 ICO 图片容器"));
        }
        let count = usize::from(u16_le(bytes, 4));
        if count == 0 || count > MAX_ICON_ENTRIES {
            return Err(invalid("条目数量必须在 1 到 1024 之间"));
        }
        let directory_end = 6 + count * 16;
        if directory_end > bytes.len() {
            return Err(invalid("尺寸目录已截断"));
        }
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            cancel.check()?;
            let offset = 6 + index * 16;
            let width = if bytes[offset] == 0 {
                256
            } else {
                u32::from(bytes[offset])
            };
            let height = if bytes[offset + 1] == 0 {
                256
            } else {
                u32::from(bytes[offset + 1])
            };
            if u16_le(bytes, offset + 4) > 1 {
                return Err(invalid("颜色平面数量无效"));
            }
            let declared_depth = u16_le(bytes, offset + 6);
            if !matches!(declared_depth, 0 | 1 | 2 | 4 | 8 | 16 | 24 | 32 | 48 | 64) {
                return Err(invalid("目录位深不受支持"));
            }
            let length = u32_le(bytes, offset + 8) as usize;
            let start = u32_le(bytes, offset + 12) as usize;
            let end = start
                .checked_add(length)
                .ok_or_else(|| invalid("条目范围溢出"))?;
            if length < 12 || start < directory_end || end > bytes.len() {
                return Err(invalid("条目偏移或长度越界"));
            }
            let payload = &bytes[start..end];
            let (encoding, bit_depth) = if payload.starts_with(PNG_SIGNATURE) {
                if payload.len() < 33 || payload[8..16] != [0, 0, 0, 13, b'I', b'H', b'D', b'R'] {
                    return Err(invalid("PNG 条目缺少完整 IHDR"));
                }
                let actual_width = u32::from_be_bytes(payload[16..20].try_into().unwrap());
                let actual_height = u32::from_be_bytes(payload[20..24].try_into().unwrap());
                if (actual_width, actual_height) != (width, height) {
                    return Err(invalid("PNG 尺寸与目录不一致"));
                }
                let depth = u16::from(payload[24]);
                let channels = match payload[25] {
                    0 if matches!(depth, 1 | 2 | 4 | 8 | 16) => 1,
                    2 if matches!(depth, 8 | 16) => 3,
                    3 if matches!(depth, 1 | 2 | 4 | 8) => 1,
                    4 if matches!(depth, 8 | 16) => 2,
                    6 if matches!(depth, 8 | 16) => 4,
                    _ => return Err(invalid("PNG 条目颜色类型或位深无效")),
                };
                // PNG's IHDR is authoritative. Real-world writers sometimes keep a 32-bit
                // directory hint for an RGB or indexed PNG, and our PNG path supports both.
                (IconEncoding::Png, depth * channels)
            } else {
                let header = u32_le(payload, 0) as usize;
                let (actual_width, doubled_height, depth, planes) = match header {
                    12 => (
                        u32::from(u16_le(payload, 4)),
                        u32::from(u16_le(payload, 6)),
                        u16_le(payload, 10),
                        u16_le(payload, 8),
                    ),
                    40 | 52 | 56 | 108 | 124 if payload.len() >= header => (
                        u32_le(payload, 4),
                        u32_le(payload, 8),
                        u16_le(payload, 14),
                        u16_le(payload, 12),
                    ),
                    _ => return Err(invalid("DIB 头不受支持或已截断")),
                };
                if (actual_width, doubled_height) != (width, height * 2) || planes != 1 {
                    return Err(invalid("DIB 尺寸或颜色平面与目录不一致"));
                }
                if !matches!(depth, 1 | 4 | 8 | 16 | 24 | 32) {
                    return Err(invalid("DIB 位深不受支持"));
                }
                (IconEncoding::Dib, depth)
            };
            if encoding == IconEncoding::Dib && declared_depth != 0 && declared_depth != bit_depth {
                return Err(invalid("条目位深与目录不一致"));
            }
            entries.push(IconEntry {
                index,
                width,
                height,
                bit_depth,
                encoding,
                range: start..end,
            });
        }
        let mut ranges: Vec<_> = entries.iter().map(|entry| entry.range.clone()).collect();
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        if ranges
            .windows(2)
            .any(|pair| pair[0].end > pair[1].start && pair[0] != pair[1])
        {
            return Err(invalid("条目数据范围相互重叠"));
        }
        // Largest area wins; equal sizes prefer higher fidelity and then the first directory row.
        let selected_index = entries
            .iter()
            .max_by_key(|entry| {
                (
                    entry.width * entry.height,
                    entry.bit_depth,
                    std::cmp::Reverse(entry.index),
                )
            })
            .unwrap()
            .index;
        Ok(Self {
            entries,
            selected_index,
        })
    }

    pub(crate) fn select(&mut self, index: Option<usize>) -> Result<()> {
        if let Some(index) = index {
            if index >= self.entries.len() {
                return Err(invalid("所选尺寸已不存在"));
            }
            self.selected_index = index;
        }
        Ok(())
    }

    pub(crate) fn select_matching(&mut self, dimensions: [u32; 2], bit_depth: u16) {
        if let Some(entry) = self
            .entries
            .iter()
            .filter(|entry| [entry.width, entry.height] == dimensions)
            .max_by_key(|entry| {
                (
                    entry.bit_depth == bit_depth,
                    entry.bit_depth,
                    std::cmp::Reverse(entry.index),
                )
            })
        {
            self.selected_index = entry.index;
        }
    }

    pub(crate) fn payload<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.entries[self.selected_index].range.clone()]
    }

    /// Normalize one validated DIB entry so the upstream ICO decoder applies its AND mask.
    pub(crate) fn single_entry(&self, bytes: &[u8], cancel: &Cancellation) -> Result<Vec<u8>> {
        let entry = &self.entries[self.selected_index];
        let payload = self.payload(bytes);
        let mut output = Vec::with_capacity(22 + payload.len());
        output.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
        output.extend_from_slice(&[(entry.width % 256) as u8, (entry.height % 256) as u8, 0, 0]);
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&entry.bit_depth.to_le_bytes());
        output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        output.extend_from_slice(&22u32.to_le_bytes());
        for chunk in payload.chunks(64 * 1024) {
            cancel.check()?;
            output.extend_from_slice(chunk);
        }
        if let Some(pixels) = legacy_zero_alpha(entry, payload) {
            // Legacy BI_RGB icons use an unused fourth byte and the AND mask for
            // transparency. Restore opaque XOR pixels in this private copy, then let
            // image's ICO decoder apply the original AND mask normally.
            for row in output[22 + pixels.start..22 + pixels.end]
                .chunks_exact_mut(entry.width as usize * 4)
            {
                cancel.check()?;
                for pixel in row.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
        }
        Ok(output)
    }
}

fn legacy_zero_alpha(entry: &IconEntry, payload: &[u8]) -> Option<Range<usize>> {
    if entry.encoding != IconEncoding::Dib || entry.bit_depth != 32 {
        return None;
    }
    let header = u32_le(payload, 0) as usize;
    if !matches!(header, 40 | 52 | 56 | 108 | 124)
        || u32_le(payload, 16) != 0
        || (header >= 56 && u32_le(payload, 52) != 0)
    {
        return None;
    }
    let end = header.checked_add(entry.width as usize * entry.height as usize * 4)?;
    let pixels = payload.get(header..end)?;
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 0)
        .then_some(header..end)
}
