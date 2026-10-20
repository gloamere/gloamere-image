//! Demand-driven animation. No decoder, queue or cache collects an entire animation.
//! Limits cover application buffers and conservative codec admission, not allocator/driver RSS.
use crate::{Cancellation, Result, ToolError, memory, pipeline};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage, metadata::Orientation};
use moxcms::{ColorProfile, DataColorSpace, Layout, Transform8BitExecutor, TransformOptions};
use std::{
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Condvar, Mutex, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_ANIMATION_PIXELS: u64 = 4_000_000;
pub const MAX_FRAMES_PER_TRAVERSAL: u32 = 2000;
pub const MAX_ANIMATION_BUFFER_BYTES: u64 = 128 * 1024 * 1024;
pub const MIN_FRAME_DELAY: Duration = Duration::from_millis(20);
const METADATA_ALLOWANCE: u64 = 8 * 1024 * 1024;
const CODEC_ALLOWANCE: u64 = 8 * 1024 * 1024;

/// Total traversals, unlike the GIF extension's count of *additional* repetitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPolicy {
    Infinite,
    Finite(u32),
}

pub fn normalize_delay(delay: Duration) -> (Duration, bool) {
    (delay.max(MIN_FRAME_DELAY), delay < MIN_FRAME_DELAY)
}

pub fn validate_animation_dimensions(width: u32, height: u32) -> Result<()> {
    pipeline::validate_dimensions(width, height)?;
    if u64::from(width) * u64::from(height) > MAX_ANIMATION_PIXELS {
        return Err(ToolError::Invalid(format!(
            "动画画布超过本版安全上限（400 万像素）：{width} × {height}"
        )));
    }
    Ok(())
}

#[derive(Clone)]
struct SharedReader {
    bytes: Arc<memory::Snapshot>,
    start: usize,
    end: usize,
    prefix: Vec<u8>,
    position: usize,
    cancel: Cancellation,
}
impl SharedReader {
    fn new(bytes: Arc<memory::Snapshot>, cancel: &Cancellation) -> Self {
        Self {
            end: bytes.len(),
            bytes,
            start: 0,
            prefix: Vec::new(),
            position: 0,
            cancel: cancel.clone(),
        }
    }
    fn frame(bytes: Arc<memory::Snapshot>, frame: &WebpFrame, cancel: &Cancellation) -> Self {
        // A virtual standalone WebP view over the original snapshot. No payload copy.
        let mut prefix = b"RIFF".to_vec();
        prefix.extend_from_slice(&((frame.end - frame.start + 22) as u32).to_le_bytes());
        prefix.extend_from_slice(b"WEBPVP8X");
        prefix.extend_from_slice(&10u32.to_le_bytes());
        prefix.extend_from_slice(&[if frame.alpha { 0x10 } else { 0 }, 0, 0, 0]);
        prefix.extend_from_slice(&(frame.width - 1).to_le_bytes()[..3]);
        prefix.extend_from_slice(&(frame.height - 1).to_le_bytes()[..3]);
        Self {
            bytes,
            start: frame.start,
            end: frame.end,
            prefix,
            position: 0,
            cancel: cancel.clone(),
        }
    }
    fn len(&self) -> usize {
        self.prefix.len() + self.end - self.start
    }
    fn check(&self) -> io::Result<()> {
        self.cancel
            .check()
            .map_err(|_| io::Error::other("动画读取已取消"))
    }
}
impl Read for SharedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let remaining = self.fill_buf()?;
        let count = remaining.len().min(out.len());
        out[..count].copy_from_slice(&remaining[..count]);
        self.position += count;
        Ok(count)
    }
}
impl BufRead for SharedReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.check()?;
        if self.position < self.prefix.len() {
            return Ok(&self.prefix[self.position..]);
        }
        let offset = self.start + (self.position - self.prefix.len()).min(self.end - self.start);
        Ok(&self.bytes[offset..self.end])
    }
    fn consume(&mut self, count: usize) {
        self.position = self.position.saturating_add(count).min(self.len());
    }
}
impl Seek for SharedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.check()?;
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => self.len() as i128 + i128::from(n),
            SeekFrom::Current(n) => self.position as i128 + i128::from(n),
        };
        self.position = usize::try_from(position).map_err(|_| io::Error::other("无效动画偏移"))?;
        Ok(self.position as u64)
    }
}
#[derive(Clone)]
struct WebpFrame {
    start: usize,
    end: usize,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    delay: u32,
    flags: u8,
    alpha: bool,
}
struct WebpInfo {
    maximum_payload: u64,
    frames: Vec<WebpFrame>,
}
// A defensive framing guard, not a second decoder. In image-webp 0.2.4 a VP8
// keyframe can allocate from its own dimensions before the ANMF mismatch is rejected.
// Walk only checked RIFF chunk ranges and fixed frame headers before calling that codec.
fn webp_preflight(bytes: &[u8], cancel: &Cancellation) -> Result<WebpInfo> {
    fn invalid() -> ToolError {
        ToolError::Invalid("WebP 容器或帧尺寸无效".into())
    }
    fn u24(b: &[u8]) -> u32 {
        u32::from_le_bytes([b[0], b[1], b[2], 0])
    }
    fn chunks(mut data: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> Result<()>) -> Result<()> {
        while !data.is_empty() {
            if data.len() < 8 {
                return Err(invalid());
            }
            let len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
            let end = 8usize.checked_add(len).ok_or_else(invalid)?;
            let padded = end.checked_add(len & 1).ok_or_else(invalid)?;
            if padded > data.len() {
                return Err(invalid());
            }
            visit(&data[..4], &data[8..end])?;
            data = &data[padded..];
        }
        Ok(())
    }
    fn bitstream(
        kind: &[u8],
        data: &[u8],
        expected: Option<(u32, u32)>,
    ) -> Result<Option<(u32, u32)>> {
        let dims = if kind == b"VP8 " {
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
            return Ok(None);
        };
        pipeline::validate_dimensions(dims.0, dims.1)?;
        if expected.is_some_and(|expected| expected != dims) {
            return Err(invalid());
        }
        Ok(Some(dims))
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(invalid());
    }
    let end = 8usize
        .checked_add(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize)
        .ok_or_else(invalid)?;
    if end > bytes.len() || end < 12 {
        return Err(invalid());
    }
    let mut canvas = None;
    let mut animated = false;
    let mut anim_seen = false;
    let mut top_bitstream_seen = false;
    let mut top_alpha_seen = false;
    let mut chunk_index = 0;
    let mut maximum_payload = 0u64;
    let mut frames = Vec::new();
    chunks(&bytes[12..end], |kind, data| {
        cancel.check()?;
        if kind == b"VP8X" {
            if data.len() != 10 || canvas.is_some() || chunk_index != 0 {
                return Err(invalid());
            }
            animated = data[0] & 2 != 0;
            let dims = (u24(&data[4..7]) + 1, u24(&data[7..10]) + 1);
            pipeline::validate_dimensions(dims.0, dims.1)?;
            if data[0] & 2 != 0 {
                validate_animation_dimensions(dims.0, dims.1)?;
            }
            canvas = Some(dims);
        } else if kind == b"ANIM" {
            if !animated || anim_seen || data.len() != 6 {
                return Err(invalid());
            }
            anim_seen = true;
        } else if kind == b"ALPH" {
            if animated || top_alpha_seen || top_bitstream_seen || canvas.is_none() {
                return Err(invalid());
            }
            top_alpha_seen = true;
        } else if kind == b"ANMF" {
            if !animated || !anim_seen || data.len() < 16 {
                return Err(invalid());
            }
            let (cw, ch) = canvas.ok_or_else(invalid)?;
            let (x, y) = (u24(&data[..3]) * 2, u24(&data[3..6]) * 2);
            let (w, h) = (u24(&data[6..9]) + 1, u24(&data[9..12]) + 1);
            if x + w > cw || y + h > ch {
                return Err(invalid());
            }
            let mut found = false;
            let mut alpha = false;
            chunks(&data[16..], |kind, data| {
                if kind == b"ALPH" {
                    if alpha || found {
                        return Err(invalid());
                    }
                    alpha = true;
                }
                if bitstream(kind, data, Some((w, h)))?.is_some() {
                    if found || (kind == b"VP8L" && alpha) {
                        return Err(invalid());
                    }
                    if kind == b"VP8L" {
                        alpha = data[4] & 0x10 != 0;
                    }
                    found = true;
                } else if kind != b"ALPH" {
                    return Err(invalid());
                }
                Ok(())
            })?;
            if !found {
                return Err(invalid());
            }
            maximum_payload = maximum_payload.max(data.len() as u64);
            if frames.len() <= MAX_FRAMES_PER_TRAVERSAL as usize {
                let start = data.as_ptr() as usize - bytes.as_ptr() as usize + 16;
                frames.push(WebpFrame {
                    start,
                    end: start + data.len() - 16,
                    x,
                    y,
                    width: w,
                    height: h,
                    delay: u24(&data[12..15]),
                    flags: data[15],
                    alpha,
                });
            }
        } else if bitstream(kind, data, canvas)?.is_some() {
            if animated || top_bitstream_seen || (kind == b"VP8L" && top_alpha_seen) {
                return Err(invalid());
            }
            top_bitstream_seen = true;
            maximum_payload = maximum_payload.max(data.len() as u64);
        }
        chunk_index += 1;
        Ok(())
    })?;
    if (animated && frames.is_empty()) || (!animated && !top_bitstream_seen) {
        return Err(invalid());
    }
    Ok(WebpInfo {
        maximum_payload,
        frames,
    })
}

fn codec_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Invalid(format!("动画解码失败：{error}"))
}
fn gif_decoder(
    bytes: Arc<memory::Snapshot>,
    cancel: &Cancellation,
) -> Result<gif::Decoder<SharedReader>> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.check_frame_consistency(true);
    options.check_lzw_end_code(true);
    // This also bounds extension metadata during read_info, before it is copied.
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        (pipeline::MAX_ICC_BYTES as u64).try_into().unwrap(),
    ));
    options
        .read_info(SharedReader::new(bytes, cancel))
        .map_err(codec_error)
}
fn webp_decoder(
    bytes: Arc<memory::Snapshot>,
    cancel: &Cancellation,
) -> Result<image_webp::WebPDecoder<SharedReader>> {
    let mut decoder =
        image_webp::WebPDecoder::new(SharedReader::new(bytes, cancel)).map_err(codec_error)?;
    // Library documents that some codec internals ignore this; canvas admission is independent.
    decoder.set_memory_limit(pipeline::MAX_ICC_BYTES);
    if decoder.is_animated() {
        // Match browsers: canvas/background disposal is transparent, not the ANIM hint.
        decoder.set_background_color([0; 4]).map_err(codec_error)?;
    }
    Ok(decoder)
}

struct Normalizer {
    orientation: Orientation,
    transform: Option<Arc<Transform8BitExecutor>>,
}
impl Normalizer {
    fn new(
        orientation: Orientation,
        icc: Option<&[u8]>,
        warnings: &mut Vec<String>,
    ) -> (Self, String, bool) {
        let had_error = warnings.iter().any(|w| w.starts_with("ICC"));
        let prepared = icc.map(|bytes| {
            let profile = pipeline::parse_icc(bytes)?;
            // GIF/WebP are decoded RGB. Applying a gray profile to synthesized luma would
            // misrepresent its source encoding; make the unsupported metadata explicit.
            if profile.color_space != DataColorSpace::Rgb {
                return Err(ToolError::Invalid(
                    "ICC 灰度配置与 GIF/WebP RGB 像素不匹配".into(),
                ));
            }
            profile
                .create_transform_8bit(
                    Layout::Rgba,
                    &ColorProfile::new_srgb(),
                    Layout::Rgba,
                    TransformOptions::default(),
                )
                .map_err(|e| ToolError::Invalid(format!("ICC 转换不可用：{e}")))
        });
        let (transform, status, managed) = match prepared {
            Some(Ok(transform)) => (Some(transform), "ICC 已转换为 sRGB", true),
            Some(Err(error)) => {
                warnings.push(error.to_string());
                (None, "ICC 未完整处理；颜色可能不准确", false)
            }
            None if had_error => (None, "ICC 核验不完整", false),
            None => (None, "无 ICC，按 sRGB 显示", true),
        };
        (
            Self {
                orientation,
                transform,
            },
            status.into(),
            managed,
        )
    }
    fn apply(&self, pixels: RgbaImage, cancel: &Cancellation) -> Result<RgbaImage> {
        cancel.check()?;
        let mut decoded = DynamicImage::ImageRgba8(pixels);
        decoded.apply_orientation(self.orientation);
        let mut pixels = decoded.into_rgba8();
        if let Some(transform) = &self.transform {
            let mut converted = RgbaImage::new(pixels.width(), pixels.height());
            let stride = pixels.width() as usize * 4;
            for (src, dst) in pixels.as_raw().chunks_exact(stride).zip(
                converted
                    .as_flat_samples_mut()
                    .samples
                    .chunks_exact_mut(stride),
            ) {
                cancel.check()?;
                transform
                    .transform(src, dst)
                    .map_err(|e| ToolError::Invalid(format!("ICC 转换失败，动画已暂停：{e}")))?;
                // Preserve unassociated alpha exactly, independent of the CMS implementation.
                for (s, d) in src
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0.iter_mut())
                {
                    d[3] = s[3];
                }
            }
            pixels = converted;
        }
        cancel.check()?;
        Ok(pixels)
    }
}

/// Immutable file snapshot plus reusable metadata. Clone the Arc; never reread the file.
pub struct AnimationSource {
    bytes: Arc<memory::Snapshot>,
    compressed_scratch: u64,
    webp_frames: Arc<Vec<WebpFrame>>,
    pub format: ImageFormat,
    pub dimensions: (u32, u32),
    pub loops: LoopPolicy,
    pub first_delay: Duration,
    pub color_status: String,
    pub color_managed: bool,
    pub warnings: Vec<String>,
    normalizer: Normalizer,
    budget: Arc<memory::Budget>,
    frames: Mutex<Vec<(Weak<memory::Pixels>, memory::Reservation)>>,
    _metadata_global: memory::Reservation,
    _metadata_local: memory::Reservation,
}
impl std::fmt::Debug for AnimationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimationSource")
            .field("format", &self.format)
            .field("dimensions", &self.dimensions)
            .field("loops", &self.loops)
            .finish_non_exhaustive()
    }
}
impl AnimationSource {
    pub fn open(self: &Arc<Self>, cancel: &Cancellation) -> Result<FrameStream> {
        cancel.check()?;
        let canvas = memory::rgba_bytes(self.dimensions.0, self.dimensions.1);
        // Raw local frame, composited canvas, orientation and ICC temporaries, codec allowance.
        // Output ownership is charged separately so a UI-held frame cannot escape the cap.
        let working = canvas * 5 + CODEC_ALLOWANCE + self.compressed_scratch;
        let local = self.budget.reserve(working)?;
        let global = memory::reserve(working)?;
        let decoder = self.decoder(cancel)?;
        Ok(FrameStream {
            source: self.clone(),
            decoder,
            cancel: cancel.clone(),
            frame_index: 0,
            traversal: 1,
            elapsed: Duration::ZERO,
            ended: false,
            working_bytes: working,
            global,
            _local: local,
        })
    }
    fn decoder(&self, cancel: &Cancellation) -> Result<Decoder> {
        match self.format {
            ImageFormat::Gif => Ok(Decoder::Gif {
                decoder: Box::new(gif_decoder(self.bytes.clone(), cancel)?),
                canvas: RgbaImage::new(self.dimensions.0, self.dimensions.1),
            }),
            ImageFormat::WebP => Ok(Decoder::WebP {
                bytes: self.bytes.clone(),
                frames: self.webp_frames.clone(),
                next: 0,
                canvas: RgbaImage::new(self.dimensions.0, self.dimensions.1),
                dispose: None,
            }),
            _ => Err(ToolError::Invalid("不支持的动画格式".into())),
        }
    }
    fn output_reservation(&self) -> Result<memory::Reservation> {
        let mut frames = self.frames.lock().unwrap();
        frames.retain(|(pixels, _)| pixels.strong_count() != 0);
        // One displayed frame plus one prepared frame. Retaining an obsolete frame pauses
        // decoding safely instead of accumulating an accidental cache across traversals.
        if frames.len() >= 2 {
            return Err(ToolError::ResourceBusy(
                "动画仍持有两帧；请释放旧帧后重试".into(),
            ));
        }
        self.budget
            .reserve(memory::rgba_bytes(self.dimensions.0, self.dimensions.1))
    }
    pub fn buffered_bytes(&self) -> u64 {
        self.frames
            .lock()
            .unwrap()
            .retain(|(pixels, _)| pixels.strong_count() != 0);
        self.budget.in_use()
    }
}

/// Called before the static JPEG/PNG path. Probe only headers, never collect decoded frames.
pub(crate) fn try_load(
    bytes: Arc<memory::Snapshot>,
    path: &Path,
    format: ImageFormat,
    cancel: &Cancellation,
    started: Instant,
) -> Result<Option<pipeline::LoadedImage>> {
    if !matches!(format, ImageFormat::Gif | ImageFormat::WebP) {
        return Ok(None);
    }
    let webp_info = if format == ImageFormat::WebP {
        webp_preflight(&bytes, cancel)?
    } else {
        WebpInfo {
            maximum_payload: 0,
            frames: Vec::new(),
        }
    };
    let compressed_scratch = webp_info.maximum_payload.saturating_mul(2);
    let mut metadata_global = memory::reserve(METADATA_ALLOWANCE * 2)?;
    let mut warnings = Vec::new();
    let (dimensions, loops, orientation, icc) = match format {
        ImageFormat::Gif => {
            let mut decoder = gif_decoder(bytes.clone(), cancel)?;
            let dimensions = (u32::from(decoder.width()), u32::from(decoder.height()));
            pipeline::validate_dimensions(dimensions.0, dimensions.1)?;
            let loops = match decoder.repeat() {
                gif::Repeat::Infinite => LoopPolicy::Infinite,
                gif::Repeat::Finite(n) => LoopPolicy::Finite(u32::from(n) + 1),
            };
            let icc = decoder.icc_profile().and_then(|data| {
                if data.len() > pipeline::MAX_ICC_BYTES {
                    warnings.push("ICC 超过 4 MiB 安全限制".into());
                    None
                } else {
                    Some(data.to_vec())
                }
            });
            let Some(info) = decoder.next_frame_info().map_err(codec_error)? else {
                return Err(ToolError::Invalid("GIF 没有可显示帧".into()));
            };
            // Consume the first frame into a reusable row, not a second canvas. The GIF
            // codec cannot advance to the next header until its current pixels are read.
            let (width, height) = (usize::from(info.width), usize::from(info.height));
            let mut row = vec![0; width * 4];
            for _ in 0..height {
                cancel.check()?;
                if !decoder.fill_buffer(&mut row).map_err(codec_error)? {
                    return Err(ToolError::Invalid("GIF 首帧不完整".into()));
                }
            }
            // If the second header is malformed, retain the valid first frame and let playback
            // report the error at that boundary. A sole frame is an ordinary editable image.
            if matches!(decoder.next_frame_info(), Ok(None)) {
                return Ok(None);
            }
            (dimensions, loops, Orientation::NoTransforms, icc)
        }
        ImageFormat::WebP => {
            let mut decoder = webp_decoder(bytes.clone(), cancel)?;
            let dimensions = decoder.dimensions();
            pipeline::validate_dimensions(dimensions.0, dimensions.1)?;
            if !decoder.is_animated() {
                return Ok(None);
            }
            let loops = match decoder.loop_count() {
                image_webp::LoopCount::Forever => LoopPolicy::Infinite,
                image_webp::LoopCount::Times(n) => LoopPolicy::Finite(u32::from(n.get())),
            };
            let orientation = match decoder.exif_metadata() {
                Ok(exif) => exif
                    .as_deref()
                    .and_then(Orientation::from_exif_chunk)
                    .unwrap_or(Orientation::NoTransforms),
                Err(error) => {
                    warnings.push(format!("方向元数据无法读取：{error}"));
                    Orientation::NoTransforms
                }
            };
            let icc = match decoder.icc_profile() {
                Ok(icc) => icc,
                Err(error) => {
                    warnings.push(format!("ICC 无法读取：{error}"));
                    None
                }
            };
            (dimensions, loops, orientation, icc)
        }
        _ => unreachable!(),
    };
    validate_animation_dimensions(dimensions.0, dimensions.1)?;
    let budget = memory::Budget::new(MAX_ANIMATION_BUFFER_BYTES);
    let metadata_local = budget.reserve(METADATA_ALLOWANCE)?;
    let (normalizer, color_status, color_managed) =
        Normalizer::new(orientation, icc.as_deref(), &mut warnings);
    drop(icc);
    metadata_global.shrink(METADATA_ALLOWANCE)?;
    let mut source = Arc::new(AnimationSource {
        bytes: bytes.clone(),
        compressed_scratch,
        webp_frames: Arc::new(webp_info.frames),
        format,
        dimensions,
        loops,
        first_delay: MIN_FRAME_DELAY,
        normalizer,
        color_status,
        color_managed,
        warnings,
        budget,
        frames: Mutex::new(Vec::new()),
        _metadata_global: metadata_global,
        _metadata_local: metadata_local,
    });
    let first = {
        let mut stream = source.open(cancel)?;
        stream
            .next_frame()?
            .ok_or_else(|| ToolError::Invalid("动画没有可显示帧".into()))?
    };
    let source_mut = Arc::get_mut(&mut source).expect("first-frame stream dropped");
    source_mut.first_delay = first.delay;
    source_mut
        .warnings
        .push("动画短于 20 ms 的帧延时统一按 20 ms 播放；暂停后不追播积压帧".into());
    if first.delay_normalized {
        source_mut.warnings.push("首帧延时已规范化为 20 ms".into());
    }
    use sha2::{Digest, Sha256};
    Ok(Some(pipeline::LoadedImage {
        path: path.to_path_buf(),
        has_alpha: first.pixels.pixels().any(|p| p[3] < 255),
        pixels: first.pixels,
        format,
        file_bytes: bytes.len() as u64,
        orientation: orientation.to_exif(),
        color_status: source.color_status.clone(),
        color_managed: source.color_managed,
        source_sha256: format!("{:x}", Sha256::digest(bytes.as_ref().as_ref())),
        warnings: source.warnings.clone(),
        load_ms: started.elapsed().as_millis(),
        animation: Some(source),
    }))
}

enum Decoder {
    Gif {
        decoder: Box<gif::Decoder<SharedReader>>,
        canvas: RgbaImage,
    },
    WebP {
        bytes: Arc<memory::Snapshot>,
        frames: Arc<Vec<WebpFrame>>,
        next: usize,
        canvas: RgbaImage,
        dispose: Option<(u32, u32, u32, u32)>,
    },
}
impl Decoder {
    fn next(
        &mut self,
        allow_frame: bool,
        cancel: &Cancellation,
    ) -> Result<Option<(RgbaImage, Duration)>> {
        match self {
            Self::Gif { decoder, canvas } => {
                let Some(info) = decoder.next_frame_info().map_err(codec_error)? else {
                    return Ok(None);
                };
                if !allow_frame {
                    return Err(ToolError::Invalid(
                        "动画单次遍历超过 2000 帧，已暂停并保留当前帧".into(),
                    ));
                }
                let (width, height, left, top, disposal, delay) = (
                    u32::from(info.width),
                    u32::from(info.height),
                    u32::from(info.left),
                    u32::from(info.top),
                    info.dispose,
                    info.delay,
                );
                if width == 0
                    || height == 0
                    || left + width > canvas.width()
                    || top + height > canvas.height()
                {
                    return Err(ToolError::Invalid("GIF 局部帧超出画布".into()));
                }
                let mut raw = vec![0; width as usize * height as usize * 4];
                decoder.read_into_buffer(&mut raw).map_err(codec_error)?;
                cancel.check()?;
                let mut output = canvas.clone();
                for y in 0..height {
                    cancel.check()?;
                    for x in 0..width {
                        let offset = (y as usize * width as usize + x as usize) * 4;
                        let current = &raw[offset..offset + 4];
                        let pixel = output.get_pixel_mut(left + x, top + y);
                        if current[3] != 0 {
                            *pixel = Rgba(current.try_into().unwrap());
                        }
                        let previous = canvas.get_pixel_mut(left + x, top + y);
                        match disposal {
                            gif::DisposalMethod::Any | gif::DisposalMethod::Keep => {
                                *previous = *pixel
                            }
                            gif::DisposalMethod::Background => *previous = Rgba([0; 4]),
                            gif::DisposalMethod::Previous => {}
                        }
                    }
                }
                Ok(Some((output, Duration::from_millis(u64::from(delay) * 10))))
            }
            Self::WebP {
                bytes,
                frames,
                next,
                canvas,
                dispose,
            } => {
                let Some(frame) = frames.get(*next) else {
                    return Ok(None);
                };
                if !allow_frame {
                    return Err(ToolError::Invalid(
                        "动画单次遍历超过 2000 帧，已暂停并保留当前帧".into(),
                    ));
                }
                if frame.x + frame.width > canvas.width()
                    || frame.y + frame.height > canvas.height()
                {
                    return Err(ToolError::Invalid("WebP 局部帧超出已批准画布".into()));
                }
                let reader = SharedReader::frame(bytes.clone(), frame, cancel);
                let mut decoder = image_webp::WebPDecoder::new(reader).map_err(codec_error)?;
                decoder.set_memory_limit(pipeline::MAX_ICC_BYTES);
                if decoder.dimensions() != (frame.width, frame.height) {
                    return Err(ToolError::Invalid("WebP 局部帧尺寸不匹配".into()));
                }
                let mut raw = vec![
                    0;
                    decoder.output_buffer_size().ok_or_else(
                        || ToolError::Invalid("WebP 输出尺寸无效".into())
                    )?
                ];
                decoder.read_image(&mut raw).map_err(codec_error)?;
                cancel.check()?;
                let raw = if decoder.has_alpha() {
                    RgbaImage::from_raw(frame.width, frame.height, raw).unwrap()
                } else {
                    DynamicImage::ImageRgb8(
                        image::RgbImage::from_raw(frame.width, frame.height, raw).unwrap(),
                    )
                    .into_rgba8()
                };
                // Disposal belongs to the previous rectangle, irrespective of the current
                // frame's size or codec color type. Reset canvas per traversal.
                if let Some((x, y, w, h)) = dispose.take() {
                    for py in y..y + h {
                        cancel.check()?;
                        for px in x..x + w {
                            canvas.put_pixel(px, py, Rgba([0; 4]));
                        }
                    }
                }
                for y in 0..frame.height {
                    cancel.check()?;
                    for x in 0..frame.width {
                        let src = raw.get_pixel(x, y).0;
                        let dst = canvas.get_pixel_mut(frame.x + x, frame.y + y);
                        if frame.flags & 2 != 0 {
                            *dst = Rgba(src);
                        } else {
                            *dst = Rgba(alpha_over(src, dst.0));
                        }
                    }
                }
                if frame.flags & 1 != 0 {
                    *dispose = Some((frame.x, frame.y, frame.width, frame.height));
                }
                *next += 1;
                Ok(Some((
                    canvas.clone(),
                    Duration::from_millis(u64::from(frame.delay)),
                )))
            }
        }
    }
}

/// Exact unassociated-alpha source-over, rounded to nearest 8-bit value.
fn alpha_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    if src[3] == 0 {
        return dst;
    }
    if src[3] == 255 || dst[3] == 0 {
        return src;
    }
    let sa = u32::from(src[3]);
    let da = u32::from(dst[3]);
    let denominator = sa * 255 + da * (255 - sa);
    let mut out = [0; 4];
    for c in 0..3 {
        out[c] =
            ((u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * da * (255 - sa) + denominator / 2)
                / denominator) as u8;
    }
    out[3] = ((denominator + 127) / 255) as u8;
    out
}

#[derive(Debug)]
pub struct AnimationFrame {
    pub pixels: Arc<memory::Pixels>,
    pub delay: Duration,
    pub frame_index: u32,
    pub traversal: u32,
    /// Cumulative normalized duration preceding this frame, including earlier traversals.
    pub elapsed: Duration,
    pub delay_normalized: bool,
}
pub struct FrameStream {
    source: Arc<AnimationSource>,
    decoder: Decoder,
    cancel: Cancellation,
    frame_index: u32,
    traversal: u32,
    elapsed: Duration,
    ended: bool,
    working_bytes: u64,
    global: memory::Reservation,
    _local: memory::Reservation,
}
impl FrameStream {
    pub fn next_frame(&mut self) -> Result<Option<AnimationFrame>> {
        self.cancel.check()?;
        if self.ended {
            return Ok(None);
        }
        let result = self.next_inner();
        if result.is_err() {
            self.ended = true;
        }
        result
    }
    fn next_inner(&mut self) -> Result<Option<AnimationFrame>> {
        let output_local = self.source.output_reservation()?;
        self.global.resize(self.working_bytes)?;
        // WebP knows its frame count. Check EOF before the traversal limit so exactly 2000
        // frames is accepted and can finish/repeat without decoding an illicit 2001st frame.
        let eof =
            matches!(&self.decoder, Decoder::WebP { next, frames, .. } if *next == frames.len());
        let mut raw = if eof {
            None
        } else {
            self.decoder
                .next(self.frame_index < MAX_FRAMES_PER_TRAVERSAL, &self.cancel)?
        };
        if raw.is_none() {
            if self.frame_index == 0 {
                return Err(ToolError::Invalid("动画遍历为空".into()));
            }
            if matches!(self.source.loops, LoopPolicy::Finite(n) if self.traversal >= n) {
                self.ended = true;
                return Ok(None);
            }
            self.traversal = self
                .traversal
                .checked_add(1)
                .ok_or_else(|| ToolError::Invalid("动画循环计数超出限制".into()))?;
            self.frame_index = 0;
            // Recreate from the same snapshot, not reset_animation: the latter retains WebP's
            // prior canvas and may leak pixels from a partial last frame into a new traversal.
            self.decoder = self.source.decoder(&self.cancel)?;
            raw = self.decoder.next(true, &self.cancel)?;
        }
        let (rgba, original_delay) =
            raw.ok_or_else(|| ToolError::Invalid("动画没有有效帧".into()))?;
        let rgba = self.source.normalizer.apply(rgba, &self.cancel)?;
        let (delay, normalized) = normalize_delay(original_delay);
        self.cancel.check()?;
        let pixels = Arc::new(memory::Pixels::from_reserved(rgba, &mut self.global)?);
        pixels.prepare_display(&self.cancel)?;
        self.source
            .frames
            .lock()
            .unwrap()
            .push((Arc::downgrade(&pixels), output_local));
        self.frame_index += 1;
        let frame = AnimationFrame {
            pixels,
            delay,
            frame_index: self.frame_index,
            traversal: self.traversal,
            elapsed: self.elapsed,
            delay_normalized: normalized,
        };
        self.elapsed = self.elapsed.saturating_add(delay);
        Ok(Some(frame))
    }
}

pub struct AnimationRequest {
    pub generation: u64,
    pub request_id: u64,
    pub source: Arc<AnimationSource>,
    pub cancel: Cancellation,
    pub restart: bool,
}
pub struct AnimationResponse {
    pub generation: u64,
    pub request_id: u64,
    pub result: Result<Option<AnimationFrame>>,
}
#[derive(Default)]
struct WorkerState {
    pending: Option<AnimationRequest>,
    active_cancel: Option<Cancellation>,
    clear: bool,
    shutdown: bool,
}
/// One OS worker, one pending demand, one result. Does not retain egui Context or call it
/// while shutdown may hold egui's memory lock; the app polls only while a request is active.
pub struct AnimationWorker {
    shared: Arc<(Mutex<WorkerState>, Condvar)>,
    result: Arc<Mutex<Option<AnimationResponse>>>,
    handle: Option<JoinHandle<()>>,
}
impl AnimationWorker {
    pub fn new(_ctx: eframe::egui::Context) -> Self {
        Self::without_context()
    }
    pub fn without_context() -> Self {
        let shared = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let result = Arc::new(Mutex::new(None));
        let input = shared.clone();
        let output = result.clone();
        let handle = thread::Builder::new()
            .name("animation-decoder".into())
            .spawn(move || {
                let mut active: Option<(u64, FrameStream)> = None;
                loop {
                    let request = {
                        let (lock, wake) = &*input;
                        let mut state = lock.lock().unwrap();
                        while state.pending.is_none() && !state.shutdown && !state.clear {
                            state = wake.wait(state).unwrap();
                        }
                        if state.shutdown {
                            break;
                        }
                        if state.clear {
                            active = None;
                            state.clear = false;
                        }
                        let Some(request) = state.pending.take() else {
                            continue;
                        };
                        state.active_cancel = Some(request.cancel.clone());
                        request
                    };
                    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        request.cancel.check()?;
                        if request.restart
                            || active.as_ref().is_none_or(|(generation, stream)| {
                                *generation != request.generation
                                    || !Arc::ptr_eq(&stream.source, &request.source)
                            })
                        {
                            active = None;
                            let mut stream = request.source.open(&request.cancel)?;
                            if !request.restart {
                                let _ = stream.next_frame()?;
                            }
                            active = Some((request.generation, stream));
                        }
                        // A caller may supply a fresh request token; stop must still reach the
                        // token retained by this decoder and its SharedReader.
                        {
                            let mut state = input.0.lock().unwrap();
                            if state.clear || state.shutdown || request.cancel.check().is_err() {
                                active.as_ref().unwrap().1.cancel.cancel();
                                return Err(ToolError::Cancelled);
                            }
                            state.active_cancel = Some(active.as_ref().unwrap().1.cancel.clone());
                        }
                        active.as_mut().unwrap().1.next_frame()
                    }))
                    .unwrap_or_else(|_| {
                        Err(ToolError::Invalid("动画解码器异常，已保留当前帧".into()))
                    });
                    let (lock, _) = &*input;
                    let mut state = lock.lock().unwrap();
                    state.active_cancel = None;
                    if state.shutdown || state.clear || request.cancel.check().is_err() {
                        active = None;
                        continue;
                    }
                    if decoded.is_err() {
                        active = None;
                    }
                    *output.lock().unwrap() = Some(AnimationResponse {
                        generation: request.generation,
                        request_id: request.request_id,
                        result: decoded,
                    });
                }
            })
            .expect("animation worker creation");
        Self {
            shared,
            result,
            handle: Some(handle),
        }
    }
    pub fn request(&self, request: AnimationRequest) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        if state.shutdown {
            return;
        }
        if request.restart {
            if let Some(cancel) = &state.active_cancel {
                cancel.cancel();
            }
            state.clear = true;
        }
        if let Some(old) = state.pending.replace(request) {
            old.cancel.cancel();
        }
        wake.notify_one();
    }
    pub fn take_result(&self) -> Option<AnimationResponse> {
        self.result.lock().unwrap().take()
    }
    pub fn stop(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.clear = true;
        if let Some(cancel) = &state.active_cancel {
            cancel.cancel();
        }
        if let Some(pending) = state.pending.take() {
            pending.cancel.cancel();
        }
        self.result.lock().unwrap().take();
        wake.notify_one();
    }
    pub fn shutdown(&mut self) {
        self.stop();
        {
            let (lock, wake) = &*self.shared;
            lock.lock().unwrap().shutdown = true;
            wake.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
impl Drop for AnimationWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Monotonic accumulated deadlines; pause/resume intentionally rebases to avoid catch-up.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    deadline: Option<Instant>,
    delay: Duration,
}
impl PlaybackClock {
    pub fn new(delay: Duration) -> Self {
        Self {
            deadline: None,
            delay: normalize_delay(delay).0,
        }
    }
    pub fn resume(&mut self, now: Instant) {
        self.deadline = now.checked_add(self.delay);
    }
    pub fn pause(&mut self) {
        self.deadline = None;
    }
    pub fn is_playing(&self) -> bool {
        self.deadline.is_some()
    }
    pub fn due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
    pub fn wake_after(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
    /// Call after displaying a frame. False asks the app to pause for sustained lateness;
    /// it never spins to catch up through an unbounded number of frames.
    pub fn accept_frame(&mut self, delay: Duration, now: Instant) -> bool {
        self.delay = normalize_delay(delay).0;
        if let Some(deadline) = self.deadline {
            if now.saturating_duration_since(deadline) > Duration::from_secs(2) {
                self.pause();
                return false;
            }
            self.deadline = deadline.checked_add(self.delay);
        }
        true
    }
}
