//! Bounded RGB8 decoding with the hash-pinned libjpeg-turbo 3.1.4.1 component.
//!
//! Only the isolated pixel worker may call this module. The pack validator must
//! verify the library identity and hold its file guards for this entire call.
//! This is not the viewer's decoder. EXIF/ICC admission remains the host's job.
//! The worker's deadline and process-memory ceiling remain required: neither
//! the output bound nor the native intermediate-buffer limit is a total RSS cap.
use crate::{Result, ToolError};
use libloading::Library;
use std::{ffi::c_void, os::raw::c_int, path::Path, ptr::NonNull};

const MAX_ENCODED: usize = 64 * 1024 * 1024;
const MAX_PIXELS: usize = 16_000_000;
const MAX_DIMENSION: usize = 32_768;

// TurboJPEG 3 ABI, from the pinned release's src/turbojpeg.h. Its size_t is
// usize, tjhandle is void*, and all functions use C calling convention.
const TJINIT_DECOMPRESS: c_int = 1;
const TJPF_RGB: c_int = 0;
const STOP_ON_WARNING: c_int = 0;
const BOTTOM_UP: c_int = 1;
const SUBSAMP: c_int = 4;
const JPEG_WIDTH: c_int = 5;
const JPEG_HEIGHT: c_int = 6;
const PRECISION: c_int = 7;
const COLORSPACE: c_int = 8;
const FAST_UPSAMPLE: c_int = 9;
const FAST_DCT: c_int = 10;
const SCAN_LIMIT: c_int = 13;
const ARITHMETIC: c_int = 14;
const LOSSLESS: c_int = 15;
const MAX_MEMORY: c_int = 23;
const NATIVE_MAX_PIXELS: c_int = 24;
const SAVE_MARKERS: c_int = 25;
type Init = unsafe extern "C" fn(c_int) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Set = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type Get = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type Header = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> c_int;
type Decompress =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut u8, c_int, c_int) -> c_int;

fn invalid(message: &str) -> ToolError {
    ToolError::Invalid(format!("实验 JPEG 解码：{message}"))
}
fn unsupported(message: &str) -> ToolError {
    ToolError::Invalid(format!("不支持：实验 JPEG 解码：{message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dimensions {
    width: usize,
    height: usize,
    len: usize,
}
impl Dimensions {
    fn new(width: usize, height: usize) -> Result<Self> {
        let pixels = width.checked_mul(height);
        if width == 0
            || height == 0
            || width > MAX_DIMENSION
            || height > MAX_DIMENSION
            || pixels.is_none_or(|n| n > MAX_PIXELS)
        {
            return Err(invalid("尺寸超过 32768 边长或 1600 万像素限制"));
        }
        let len = pixels
            .and_then(|n| n.checked_mul(3))
            .ok_or_else(|| invalid("RGB 缓冲区大小溢出"))?;
        Ok(Self { width, height, len })
    }
}

/// Independently gate dimensions, precision and SOF mode before loading native
/// code or allocating pixels. Every iteration consumes at least one byte; all
/// segment accesses are checked and work is bounded by MAX_ENCODED.
fn admit_header(bytes: &[u8]) -> Result<Dimensions> {
    if bytes.len() > MAX_ENCODED {
        return Err(invalid("压缩数据超过 64 MiB 限制"));
    }
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(invalid("缺少 JPEG 起始标记"));
    }
    let mut offset = 2;
    let mut dimensions = None;
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            return Err(invalid("JPEG 头部标记损坏"));
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes
            .get(offset)
            .ok_or_else(|| invalid("JPEG 头部已截断"))?;
        offset += 1;
        if matches!(marker, 0x00 | 0x01 | 0xd0..=0xd9) {
            return Err(invalid("JPEG 头部含意外独立标记"));
        }
        let length_bytes = bytes
            .get(offset..offset + 2)
            .ok_or_else(|| invalid("JPEG 段长度已截断"))?;
        let length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if length < 2 {
            return Err(invalid("JPEG 段长度无效"));
        }
        let segment = bytes
            .get(offset + 2..offset + length)
            .ok_or_else(|| invalid("JPEG 段已截断"))?;
        offset += length;
        if marker == 0xda {
            return dimensions.ok_or_else(|| invalid("JPEG 缺少图像尺寸头"));
        }
        // All SOF markers except DHT (C4), JPG (C8), and DAC (CC).
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            if !matches!(marker, 0xc0 | 0xc2) {
                return Err(unsupported("仅支持 Huffman 基线或渐进式 JPEG"));
            }
            if dimensions.is_some() || segment.len() < 6 {
                return Err(invalid("JPEG 尺寸头重复或已截断"));
            }
            if segment[0] != 8 {
                return Err(unsupported("仅支持 8 位 JPEG"));
            }
            let components = usize::from(segment[5]);
            if segment.len() != 6 + components * 3 {
                return Err(invalid("JPEG 分量头长度无效"));
            }
            if !matches!(components, 1 | 3) {
                return Err(unsupported("仅支持 YCbCr 或灰度 JPEG"));
            }
            let height = usize::from(u16::from_be_bytes([segment[1], segment[2]]));
            let width = usize::from(u16::from_be_bytes([segment[3], segment[4]]));
            dimensions = Some(Dimensions::new(width, height)?);
        } else if matches!(marker, 0xc8 | 0xcc | 0xdc | 0xde | 0xdf) {
            return Err(unsupported("JPEG 使用未经验证的扩展或算术编码"));
        }
    }
    Err(invalid("JPEG 缺少扫描数据"))
}

/// Apply the bounded mode/depth/dimension gate before another decoder inspects
/// metadata. This does not load native code or certify that scan data is valid.
pub fn validate_header(bytes: &[u8]) -> Result<()> {
    admit_header(bytes).map(|_| ())
}

struct Api {
    init: Init,
    destroy: Destroy,
    set: Set,
    get: Get,
    header: Header,
    decompress: Decompress,
    // Own the library for at least as long as every copied function pointer.
    _library: Library,
}
impl Api {
    fn load(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("组件必须使用经过校验的绝对路径"));
        }
        // SAFETY: the caller is the pixel worker, after pinned hash/size
        // validation, with file guards held. Only the reviewed official
        // TurboJPEG ABI is loaded. Windows permits dependencies from System32
        // only; the pinned DLL imports only KERNEL32.dll.
        let library = unsafe {
            #[cfg(windows)]
            {
                libloading::os::windows::Library::load_with_flags(
                    path,
                    libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
                .map(Library::from)
            }
            #[cfg(not(windows))]
            {
                Library::new(path)
            }
        }
        .map_err(|_| invalid("无法加载已校验的 JPEG 组件"))?;
        // SAFETY: exact C signatures are copied from the pinned native header.
        // The owned library remains alive while these pointers can be called.
        unsafe {
            Ok(Self {
                init: *library.get::<Init>(b"tj3Init\0").map_err(abi_error)?,
                destroy: *library.get::<Destroy>(b"tj3Destroy\0").map_err(abi_error)?,
                set: *library.get::<Set>(b"tj3Set\0").map_err(abi_error)?,
                get: *library.get::<Get>(b"tj3Get\0").map_err(abi_error)?,
                header: *library
                    .get::<Header>(b"tj3DecompressHeader\0")
                    .map_err(abi_error)?,
                decompress: *library
                    .get::<Decompress>(b"tj3Decompress8\0")
                    .map_err(abi_error)?,
                _library: library,
            })
        }
    }
}
fn abi_error(_: libloading::Error) -> ToolError {
    invalid("组件缺少必需的 TurboJPEG 3 ABI")
}

struct Decoder<'a> {
    api: &'a Api,
    handle: NonNull<c_void>,
}
impl<'a> Decoder<'a> {
    fn new(api: &'a Api) -> Result<Self> {
        // SAFETY: valid initialization constant, with the library kept alive.
        let handle = NonNull::new(unsafe { (api.init)(TJINIT_DECOMPRESS) })
            .ok_or_else(|| invalid("无法初始化 JPEG 解码器"))?;
        Ok(Self { api, handle })
    }
    fn set(&self, parameter: c_int, value: c_int) -> Result<()> {
        // SAFETY: a live decompressor handle and documented integer parameter.
        if unsafe { (self.api.set)(self.handle.as_ptr(), parameter, value) } != 0 {
            return Err(invalid("JPEG 解码安全参数设置失败"));
        }
        Ok(())
    }
    fn get(&self, parameter: c_int) -> c_int {
        // SAFETY: the handle and function pointer share this library lifetime.
        unsafe { (self.api.get)(self.handle.as_ptr(), parameter) }
    }
}
impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: uniquely owned initialized handle; Api outlives Decoder.
        unsafe { (self.api.destroy)(self.handle.as_ptr()) }
    }
}

/// Decode admitted 8-bit lossy Huffman YCbCr/grayscale JPEG to full-size RGB8.
/// The host supplies a hash-verified library and retains its file guards until
/// return. Accurate IDCT, fancy upsampling, top-down rows, and scale 1/1 are
/// intentional parts of the Pillow parity contract. No metadata is transformed.
pub fn decode_rgb(bytes: &[u8], library: &Path) -> Result<image::RgbImage> {
    let dimensions = admit_header(bytes)?;
    let api = Api::load(library)?;
    let decoder = Decoder::new(&api)?;
    for (parameter, value) in [
        (STOP_ON_WARNING, 1),
        (BOTTOM_UP, 0),
        (FAST_UPSAMPLE, 0),
        (FAST_DCT, 0),
        (SCAN_LIMIT, 500),
        (MAX_MEMORY, 128),
        (NATIVE_MAX_PIXELS, MAX_PIXELS as c_int),
        (SAVE_MARKERS, 0),
    ] {
        decoder.set(parameter, value)?;
    }
    // SAFETY: bytes is immutable and lives through this synchronous call, and
    // its exact bounded length is supplied. No output allocation occurs here.
    if unsafe { (api.header)(decoder.handle.as_ptr(), bytes.as_ptr(), bytes.len()) } != 0 {
        return Err(invalid("JPEG 头部损坏或不受支持"));
    }
    if decoder.get(JPEG_WIDTH) != dimensions.width as c_int
        || decoder.get(JPEG_HEIGHT) != dimensions.height as c_int
    {
        return Err(invalid("JPEG 头部尺寸不一致"));
    }
    if decoder.get(PRECISION) != 8
        || decoder.get(ARITHMETIC) != 0
        || decoder.get(LOSSLESS) != 0
        // TJCS_YCbCr=1, TJCS_GRAY=2. Direct RGB/CMYK/YCCK are unvalidated.
        || !matches!(decoder.get(COLORSPACE), 1 | 2)
        // TJSAMP_444=0, 422=1, 420=2, GRAY=3.
        || !matches!(decoder.get(SUBSAMP), 0..=3)
    {
        return Err(unsupported("JPEG 色彩、采样或精度超出已验证范围"));
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(dimensions.len)
        .map_err(|_| ToolError::ResourceBusy("无法分配 JPEG RGB 缓冲区".into()))?;
    pixels.resize(dimensions.len, 0);
    // SAFETY: same immutable bytes/handle as the checked header. A fresh handle
    // retains native scale 1/1 and no cropping. RGB rows are width*3, and the
    // owned buffer has exactly width*height*3 initialized bytes. Width <=32768,
    // so pitch fits c_int. No references alias the mutable destination.
    if unsafe {
        (api.decompress)(
            decoder.handle.as_ptr(),
            bytes.as_ptr(),
            bytes.len(),
            pixels.as_mut_ptr(),
            (dimensions.width * 3) as c_int,
            TJPF_RGB,
        )
    } != 0
    {
        return Err(invalid("JPEG 数据损坏、截断或超过解码资源限制"));
    }
    image::RgbImage::from_raw(dimensions.width as u32, dimensions.height as u32, pixels)
        .ok_or_else(|| invalid("JPEG 输出尺寸与 RGB 缓冲区不一致"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(marker: u8, precision: u8, width: u16, height: u16, components: u8) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8, 0xff, marker];
        out.extend_from_slice(&(8 + u16::from(components) * 3).to_be_bytes());
        out.push(precision);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.push(components);
        for component in 1..=components {
            out.extend_from_slice(&[component, 0x11, 0]);
        }
        out.extend_from_slice(&[0xff, 0xda, 0, 2]);
        out
    }
    #[test]
    fn parser_accepts_only_eight_bit_baseline_progressive_gray_or_three_components() {
        for marker in [0xc0, 0xc2] {
            for components in [1, 3] {
                let d = admit_header(&header(marker, 8, 227, 229, components)).unwrap();
                assert_eq!(d.len, 227 * 229 * 3);
            }
        }
        for marker in [
            0xc1, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
        ] {
            assert!(admit_header(&header(marker, 8, 227, 229, 3)).is_err());
        }
        for precision in [0, 1, 7, 9, 12, 16] {
            assert!(admit_header(&header(0xc0, precision, 227, 229, 3)).is_err());
        }
        for components in [0, 2, 4, 255] {
            assert!(admit_header(&header(0xc0, 8, 227, 229, components)).is_err());
        }
    }
    #[test]
    fn excessive_dimensions_are_rejected_before_library_load() {
        for (width, height) in [(0, 1), (1, 0), (32769, 1), (1, 32769), (4001, 4000)] {
            let err =
                decode_rgb(&header(0xc0, 8, width, height, 3), Path::new("missing")).unwrap_err();
            assert!(matches!(err, ToolError::Invalid(ref text) if text.contains("尺寸超过")));
        }
        assert!(admit_header(&header(0xc0, 8, 32768, 1, 3)).is_ok());
        assert!(admit_header(&header(0xc0, 8, 4000, 4000, 3)).is_ok());
    }
    #[test]
    fn every_header_truncation_is_an_error_without_panicking() {
        let full = header(0xc2, 8, 227, 229, 3);
        for length in 0..full.len() {
            assert!(admit_header(&full[..length]).is_err(), "length={length}");
        }
        let mut duplicate = full[..full.len() - 4].to_vec();
        duplicate.extend_from_slice(&full[2..]);
        assert!(admit_header(&duplicate).is_err());
        for malformed in [
            &[0xff, 0xd8, 0xff, 0xff][..],
            &[0xff, 0xd8, 0xff, 0xe0, 0, 0],
            &[0xff, 0xd8, 0xff, 0xe0, 0, 1],
            &[0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff],
            &[0xff, 0xd8, 0xff, 0xd9],
        ] {
            assert!(admit_header(malformed).is_err());
        }
    }
    #[test]
    fn encoded_limit_is_rejected_before_library_load() {
        let bytes = vec![0; MAX_ENCODED + 1];
        assert!(matches!(decode_rgb(&bytes, Path::new("missing")),
            Err(ToolError::Invalid(ref text)) if text.contains("64 MiB")));
    }
    #[test]
    fn relative_library_path_is_never_searched() {
        assert!(
            matches!(decode_rgb(&header(0xc0, 8, 1, 1, 3), Path::new("turbojpeg.dll")),
            Err(ToolError::Invalid(ref text)) if text.contains("绝对路径"))
        );
    }

    #[test]
    fn unsupported_modes_are_distinct_from_malformed_inputs() {
        for bytes in [
            header(0xc3, 8, 227, 229, 3),
            header(0xc0, 12, 227, 229, 3),
            header(0xc0, 8, 227, 229, 4),
        ] {
            assert!(matches!(admit_header(&bytes),
                Err(ToolError::Invalid(ref message)) if message.starts_with("不支持：")));
        }
        assert!(matches!(admit_header(&[0xff, 0xd8, 0xff]),
            Err(ToolError::Invalid(ref message)) if !message.starts_with("不支持：")));
    }
}
