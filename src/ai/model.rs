//! Verified experimental CoDE component and isolated ONNX CPU execution.
//! Neither the reference zero boundary nor model logits are quality approval.
use super::types::ModelResult;
use crate::{Cancellation, Result, ToolError};
use image::{ImageDecoder, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub const PREPROCESS: &str = "code-rgb-center224-imagenet-ljt3141-v2";
pub const MODEL_HASH: &str = "5a888282941391e2249af4da8af54ee36a822ff3d09c22f2e6cb4a6be6dc2e7b";
pub const MODEL_BYTES: u64 = 22_278_043;
pub const WORKER_PROTOCOL: u32 = 1;
pub const RESEARCH_THRESHOLD: f64 = 0.0;
pub const RESEARCH_POLICY: &str = "experimental-zero-v1";

/// Shipped quality policy; imports, settings and cache cannot grant this gate.
pub fn stable_approved() -> bool {
    false
}

pub fn fingerprint() -> String {
    let runtime = expected_runtime()
        .map(|(_, hash, _)| hash)
        .unwrap_or("unsupported");
    let decoder = expected_jpeg_decoder()
        .map(|v| v.sha256)
        .unwrap_or_default();
    format!(
        "{MODEL_HASH}:{PREPROCESS}:ort1283:{runtime}:jpeg:{decoder}:protocol{WORKER_PROTOCOL}:{RESEARCH_POLICY}"
    )
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub model_id: String,
    pub runtime_version: String,
    pub installed_bytes: u64,
    pub license: &'static str,
    pub source_revision: String,
}

pub fn component_info(pack: &VerifiedPack) -> Result<ComponentInfo> {
    let mut installed_bytes = 0u64;
    for name in std::iter::once("manifest.json")
        .chain(std::iter::once(pack.manifest.model_file.as_str()))
        .chain(std::iter::once(pack.manifest.runtime_file.as_str()))
        .chain(std::iter::once(pack.manifest.jpeg_decoder.name.as_str()))
        .chain(
            pack.manifest
                .runtime_support
                .iter()
                .map(|v| v.name.as_str()),
        )
        .chain(pack.manifest.license_files.iter().map(String::as_str))
    {
        installed_bytes += std::fs::metadata(pack.directory.join(name))?.len();
    }
    Ok(ComponentInfo {
        model_id: pack.manifest.model_id.clone(),
        runtime_version: pack.manifest.runtime_version.clone(),
        installed_bytes,
        license: "CoDE MIT；ONNX Runtime MIT；JPEG 组件许可及第三方声明见离线包",
        source_revision: pack.manifest.source_revision.clone(),
    })
}

/// Fixed synthetic input, no user image is read. Exercises the real isolated
/// worker, preprocessing and pinned runtime before a component becomes ready.
pub fn validate_and_self_test(path: &Path) -> Result<VerifiedPack> {
    let pack = validate_pack(path)?;
    let fixture = image::RgbImage::from_fn(224, 224, |x, y| {
        image::Rgb([
            ((x * 13 + y * 7) % 256) as u8,
            ((x * 3 + y * 17) % 256) as u8,
            ((x + y * 5) % 256) as u8,
        ])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(fixture)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| invalid(e.to_string()))?;
    let result = infer_in_worker(path, bytes.get_ref(), &Cancellation::default())?;
    // Reference from the independently pinned v0.6 worker. A generous numeric
    // tolerance permits CPU kernel variation; this is not a classifier threshold.
    if (result.raw_score - 0.25280970335006714).abs() > 0.01 {
        return Err(invalid("组件固定自检分值不匹配；未启用像素检测"));
    }
    Ok(pack)
}

const MAX_REPORT: u64 = 16 * 1024;
const LICENSE_FILES: [&str; 9] = [
    "CoDE-MIT.txt",
    "CoDE-model-card-upstream.md",
    "ONNXRuntime-LICENSE",
    "ONNXRuntime-ThirdPartyNotices.txt",
    "ONNXRuntime-Privacy.md",
    "MODEL-PROVENANCE.json",
    "libjpeg-turbo-LICENSE.md",
    "libjpeg-turbo-README.ijg",
    "JPEG-PROVENANCE.json",
];
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentFile {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}
fn support_files() -> Vec<ComponentFile> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        vec![ComponentFile {
            name: "onnxruntime_providers_shared.dll".into(),
            sha256: "6582418469facb25b33bc212d8efdc8f7ee57d0c8161e57a59434377780a38b4".into(),
            bytes: 21856,
        }]
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        Vec::new()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub schema_version: u32,
    pub model_id: String,
    pub model_file: String,
    pub model_sha256: String,
    pub model_bytes: u64,
    pub runtime_file: String,
    pub runtime_sha256: String,
    pub runtime_bytes: u64,
    pub runtime_version: String,
    pub runtime_support: Vec<ComponentFile>,
    pub jpeg_decoder: ComponentFile,
    pub preprocess_version: String,
    pub source_revision: String,
    pub license_files: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct VerifiedPack {
    pub directory: PathBuf,
    pub manifest: PackManifest,
}
fn invalid(s: impl Into<String>) -> ToolError {
    ToolError::Invalid(s.into())
}
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("组件只接受普通文件"));
    }
    let mut out = Vec::new();
    file.take(max + 1).read_to_end(&mut out)?;
    if out.len() as u64 > max {
        return Err(invalid("组件文件超过大小限制"));
    }
    Ok(out)
}
fn basename(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && !s.contains(['/', '\\', ':']) && s != "." && s != ".."
}
fn expected_runtime() -> Option<(&'static str, &'static str, u64)> {
    // Populated only from independently verified official Microsoft release files.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return runtime_hashes::LINUX;
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        return runtime_hashes::WINDOWS;
    }
    #[allow(unreachable_code)]
    None
}
mod runtime_hashes {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub const LINUX: Option<(&str, &str, u64)> = Some((
        "libonnxruntime.so.1.28.3",
        "5a1ce74e56e8c4b278d5a9e9d12b7192d9f70811084dde9616da4fa1710efc5a",
        24_301_616,
    ));
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub const WINDOWS: Option<(&str, &str, u64)> = Some((
        "onnxruntime.dll",
        "4d2774a5f64e4a230b16b74b167171e92f65b0683c19680c1d04fb284b92a6e2",
        15_828_832,
    ));
}
fn expected_jpeg_decoder() -> Option<ComponentFile> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some(ComponentFile {
            name: "libturbojpeg.so.0.4.0".into(),
            sha256: "a7a6fd79fd6ab9b294904723c5e1a340c8a2be1c0aa05125e2cc3b27a66aa60c".into(),
            bytes: 907408,
        });
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        return Some(ComponentFile {
            name: "turbojpeg.dll".into(),
            sha256: "4a171bfe14f3032fcaad3db6c9a199b0d81cf6ca8ce3b6f4c98809df76fadeb8".into(),
            bytes: 1133568,
        });
    }
    #[allow(unreachable_code)]
    None
}
pub fn license_files() -> &'static [&'static str] {
    &LICENSE_FILES
}
pub fn trusted_component_files() -> Vec<ComponentFile> {
    let mut files = vec![ComponentFile {
        name: "code-linear-fp32.onnx".into(),
        sha256: MODEL_HASH.into(),
        bytes: MODEL_BYTES,
    }];
    if let Some((name, hash, bytes)) = expected_runtime() {
        files.push(ComponentFile {
            name: name.into(),
            sha256: hash.into(),
            bytes,
        });
    }
    if let Some(jpeg) = expected_jpeg_decoder() {
        files.push(jpeg);
    }
    files.extend(support_files());
    files
}

pub fn validate_pack(path: &Path) -> Result<VerifiedPack> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("组件目录不接受符号链接"));
    }
    let directory = path.canonicalize()?;
    let manifest: PackManifest =
        serde_json::from_slice(&read_bounded(&directory.join("manifest.json"), 16 * 1024)?)
            .map_err(|e| invalid(format!("组件清单无效：{e}")))?;
    let Some((runtime_name, runtime_hash, runtime_bytes)) = expected_runtime() else {
        return Err(invalid("此平台尚无已核验的实验运行时包"));
    };
    if manifest.schema_version != 2
        || Some(manifest.jpeg_decoder.clone()) != expected_jpeg_decoder()
        || manifest.model_id != "code-linear-fp32-experimental"
        || manifest.model_file != "code-linear-fp32.onnx"
        || manifest.model_sha256 != MODEL_HASH
        || manifest.model_bytes != MODEL_BYTES
        || manifest.preprocess_version != PREPROCESS
        || manifest.source_revision != "c2a0c9216cd8b96e91927848cb461ecbdfc5a329"
        || manifest.runtime_file != runtime_name
        || manifest.runtime_sha256 != runtime_hash
        || manifest.runtime_bytes != runtime_bytes
        || manifest.runtime_version != "1.28.3"
    {
        return Err(invalid("组件不是内置受信清单中的实验版本；拒绝加载"));
    }
    for (name, hash, size) in [
        (&manifest.model_file, MODEL_HASH, MODEL_BYTES),
        (&manifest.runtime_file, runtime_hash, runtime_bytes),
        (
            &manifest.jpeg_decoder.name,
            manifest.jpeg_decoder.sha256.as_str(),
            manifest.jpeg_decoder.bytes,
        ),
    ] {
        if !basename(name) {
            return Err(invalid("组件路径不安全"));
        }
        let p = directory.join(name);
        if std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
            return Err(invalid("组件不接受符号链接"));
        }
        let bytes = read_bounded(&p, size)?;
        if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(&bytes)) != hash {
            return Err(invalid("组件 SHA-256 或大小校验失败"));
        }
    }
    if manifest.runtime_support != support_files() {
        return Err(invalid("组件辅助运行时清单不匹配"));
    }
    for item in &manifest.runtime_support {
        let p = directory.join(&item.name);
        if std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
            return Err(invalid("组件辅助运行时拒绝符号链接"));
        }
        let bytes = read_bounded(&p, item.bytes)?;
        if bytes.len() as u64 != item.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != item.sha256
        {
            return Err(invalid("组件辅助运行时 SHA-256 不匹配"));
        }
    }
    if manifest.license_files != LICENSE_FILES {
        return Err(invalid("组件许可清单不匹配"));
    }
    for name in &manifest.license_files {
        if !basename(name) {
            return Err(invalid("组件许可路径无效"));
        }
        let file = directory.join(name);
        if std::fs::symlink_metadata(&file)?.file_type().is_symlink()
            || read_bounded(&file, 1024 * 1024)?.is_empty()
        {
            return Err(invalid("组件许可文件无效"));
        }
    }
    Ok(VerifiedPack {
        directory,
        manifest,
    })
}
pub fn install_pack(source: &Path, target: &Path) -> Result<()> {
    let verified = validate_pack(source)?;
    let parent = target.parent().ok_or_else(|| invalid("组件目录无效"))?;
    std::fs::create_dir_all(parent)?;
    if target.exists() {
        return Err(invalid("同版本组件已经存在；请先移除，再重新导入"));
    }
    let stage = tempfile::tempdir_in(parent)?;
    for name in std::iter::once("manifest.json")
        .chain(std::iter::once(verified.manifest.model_file.as_str()))
        .chain(std::iter::once(verified.manifest.runtime_file.as_str()))
        .chain(std::iter::once(
            verified.manifest.jpeg_decoder.name.as_str(),
        ))
        .chain(
            verified
                .manifest
                .runtime_support
                .iter()
                .map(|f| f.name.as_str()),
        )
        .chain(verified.manifest.license_files.iter().map(String::as_str))
    {
        std::fs::copy(verified.directory.join(name), stage.path().join(name))?;
    }
    validate_and_self_test(stage.path())?;
    std::fs::rename(stage.path(), target)?;
    Ok(())
}
pub fn remove_pack(path: &Path) -> Result<()> {
    // Removal trusts a fixed inventory, never arbitrary filenames from JSON.
    // Corrupt model bytes remain removable; symlinked component directories do not.
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("组件目录不接受符号链接"));
    }
    let directory = path.canonicalize()?;
    let manifest: PackManifest =
        serde_json::from_slice(&read_bounded(&directory.join("manifest.json"), 16384)?)
            .map_err(|e| invalid(e.to_string()))?;
    let (runtime_name, _, _) = expected_runtime().ok_or_else(|| invalid("此平台不支持组件清理"))?;
    if manifest.model_id != "code-linear-fp32-experimental"
        || manifest.model_file != "code-linear-fp32.onnx"
        || manifest.runtime_file != runtime_name
        || manifest.runtime_support != support_files()
        || Some(manifest.jpeg_decoder.clone()) != expected_jpeg_decoder()
        || manifest.license_files != LICENSE_FILES
    {
        return Err(invalid("组件清理清单不匹配；未删除文件"));
    }
    for name in std::iter::once("code-linear-fp32.onnx")
        .chain(std::iter::once(runtime_name))
        .chain(std::iter::once(manifest.jpeg_decoder.name.as_str()))
        .chain(manifest.runtime_support.iter().map(|f| f.name.as_str()))
        .chain(LICENSE_FILES)
    {
        let item = directory.join(name);
        match std::fs::remove_file(&item) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    std::fs::remove_file(directory.join("manifest.json"))?;
    let _ = std::fs::remove_dir(&directory); // Unknown files are retained.
    Ok(())
}
/// torchvision CenterCrop uses round(), i.e. nearest-even for half coordinates.
pub fn crop_offset(length: u32) -> u32 {
    let delta = length - 224;
    delta / 2 + u32::from(delta % 2 == 1 && (delta / 2) % 2 == 1)
}
pub fn preprocess_rgb8(rgb: &image::RgbImage) -> Result<Vec<f32>> {
    let (w, h) = rgb.dimensions();
    if w < 224 || h < 224 || w > h.saturating_mul(4) || h > w.saturating_mul(4) {
        return Err(invalid("不支持：短边小于 224 或长宽比超过 4:1；未分析像素"));
    }
    let (x, y) = (crop_offset(w), crop_offset(h));
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut tensor = vec![0f32; 3 * 224 * 224];
    for c in 0..3 {
        for row in 0..224 {
            for col in 0..224 {
                tensor[c * 224 * 224 + row * 224 + col] =
                    (f32::from(rgb.get_pixel(x + col as u32, y + row as u32)[c]) / 255.0 - mean[c])
                        / std[c];
            }
        }
    }
    Ok(tensor)
}
fn input_error(error: image::ImageError) -> ToolError {
    if matches!(error, image::ImageError::Limits(_)) {
        invalid("不支持：图片超过像素检测尺寸或解码预算")
    } else {
        error.into()
    }
}

/// Cheap supported-format/header gate before a pixel worker is scheduled.
/// Transparency is checked after bounded decoding inside the isolated worker.
pub fn preflight_input(bytes: &[u8]) -> Result<()> {
    let format = image::guess_format(bytes)?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err(invalid(
            "不支持：仅静态 8 位不透明 JPEG / PNG 可做实验像素检测",
        ));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::pipeline::MAX_DIMENSION);
    limits.max_image_height = Some(crate::pipeline::MAX_DIMENSION);
    limits.max_alloc = Some(crate::pipeline::MAX_DECODE_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(input_error)?;
    let (w, h) = decoder.dimensions();
    if crate::pipeline::validate_dimensions(w, h).is_err()
        || decoder.total_bytes() > crate::pipeline::MAX_DECODE_BYTES
        || w < 224
        || h < 224
        || w > h.saturating_mul(4)
        || h > w.saturating_mul(4)
    {
        return Err(invalid(
            "不支持：像素尺寸、内存预算或 4:1 长宽比超出实验范围",
        ));
    }
    if decoder.orientation()? != image::metadata::Orientation::NoTransforms
        || decoder.icc_profile()?.is_some()
    {
        return Err(invalid(
            "不支持：EXIF 方向或 ICC 输入尚未完成模型一致性验证",
        ));
    }
    if format == image::ImageFormat::Png {
        if image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits)
            .map_err(input_error)?
            .is_apng()?
        {
            return Err(invalid("不支持：动画图片不能用单帧代表整幅动画"));
        }
        if !matches!(
            decoder.color_type(),
            image::ColorType::Rgb8 | image::ColorType::Rgba8
        ) {
            return Err(invalid("不支持：实验组件暂仅验证 8 位 RGB 输入"));
        }
    } else {
        super::jpeg::validate_header(bytes)?;
    }
    Ok(())
}

pub fn prepare_tensor(bytes: &[u8], jpeg_library: &Path) -> Result<Vec<f32>> {
    preflight_input(bytes)?;
    let format = image::guess_format(bytes)?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err(invalid(
            "不支持：实验像素组件只接收静态 8 位 JPEG / RGB PNG",
        ));
    }
    if format == image::ImageFormat::Jpeg {
        super::jpeg::validate_header(bytes)?;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(crate::pipeline::MAX_DIMENSION);
    limits.max_image_height = Some(crate::pipeline::MAX_DIMENSION);
    limits.max_alloc = Some(crate::pipeline::MAX_DECODE_BYTES);
    if format == image::ImageFormat::Png
        && image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits.clone())?
            .is_apng()?
    {
        return Err(invalid("不支持：动画图片不能用单帧代表整幅动画"));
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(input_error)?;
    let (w, h) = decoder.dimensions();
    crate::pipeline::validate_dimensions(w, h)?;
    if decoder.orientation()? != image::metadata::Orientation::NoTransforms
        || decoder.icc_profile()?.is_some()
    {
        return Err(invalid(
            "不支持：EXIF 方向或 ICC 输入尚未完成模型一致性验证",
        ));
    }
    if format == image::ImageFormat::Jpeg {
        drop(decoder);
        return preprocess_rgb8(&super::jpeg::decode_rgb(bytes, jpeg_library)?);
    }
    if !matches!(
        decoder.color_type(),
        image::ColorType::Rgb8 | image::ColorType::Rgba8
    ) {
        return Err(invalid("不支持：实验组件暂仅验证 8 位 RGB 输入"));
    }
    let image = image::DynamicImage::from_decoder(decoder)?;
    if image.to_rgba8().pixels().any(|p| p[3] != 255) {
        return Err(invalid("不支持：透明图片尚未验证，未合成背景进行推断"));
    }
    preprocess_rgb8(&image.to_rgb8())
}
#[derive(Serialize, Deserialize)]
struct WorkerReply {
    protocol: u8,
    file_sha256: String,
    model_sha256: String,
    preprocess: String,
    raw_score: Option<f64>,
    error: Option<String>,
}
/// Called before GUI initialization; hidden child has no clipboard or network input.
pub fn worker_main(pack: &Path) -> Result<()> {
    let mut length = [0; 8];
    std::io::stdin().read_exact(&mut length)?;
    let n = u64::from_le_bytes(length);
    if n == 0 || n > crate::pipeline::MAX_INPUT_BYTES {
        return Err(invalid("检测快照大小无效"));
    }
    let mut bytes = vec![0; n as usize];
    std::io::stdin().read_exact(&mut bytes)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let result = (|| -> Result<f64> {
        let p = validate_pack(pack)?;
        #[cfg(windows)]
        super::windows::harden_dll_search()?;
        #[cfg(windows)]
        let _jpeg_lock = {
            use std::os::windows::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(p.directory.join(&p.manifest.jpeg_decoder.name))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if format!("{:x}", Sha256::digest(&bytes)) != p.manifest.jpeg_decoder.sha256 {
                return Err(invalid("JPEG 解码器映射前校验失败"));
            }
            file
        };
        let tensor = prepare_tensor(&bytes, &p.directory.join(&p.manifest.jpeg_decoder.name))?;
        #[cfg(windows)]
        let _runtime_lock = {
            use std::os::windows::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(p.directory.join(&p.manifest.runtime_file))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if format!("{:x}", Sha256::digest(&bytes)) != p.manifest.runtime_sha256 {
                return Err(invalid("运行时映射前校验失败"));
            }
            file
        };
        // On Unix, app-data must not be concurrently modified by another process.
        // A process-isolation boundary is not an OS sandbox against that account.
        ort::init_from(p.directory.join(&p.manifest.runtime_file))
            .map_err(|e| invalid(e.to_string()))?
            .with_telemetry(false)
            .commit();
        let model_bytes = read_bounded(&p.directory.join(&p.manifest.model_file), MODEL_BYTES)?;
        if model_bytes.len() as u64 != MODEL_BYTES
            || format!("{:x}", Sha256::digest(&model_bytes)) != MODEL_HASH
        {
            return Err(invalid("实际推理缓冲区的模型校验失败"));
        }
        let mut session = ort::session::Session::builder()
            .map_err(|e| invalid(e.to_string()))?
            .with_intra_threads(2)
            .map_err(|e| invalid(e.to_string()))?
            .with_inter_threads(1)
            .map_err(|e| invalid(e.to_string()))?
            .with_intra_op_spinning(false)
            .map_err(|e| invalid(e.to_string()))?
            .with_inter_op_spinning(false)
            .map_err(|e| invalid(e.to_string()))?
            .commit_from_memory(&model_bytes)
            .map_err(|e| invalid(e.to_string()))?;
        let input =
            ort::value::Tensor::from_array(([1usize, 3, 224, 224], tensor.into_boxed_slice()))
                .map_err(|e| invalid(e.to_string()))?;
        let outputs = session
            .run(ort::inputs!["pixel_values"=>input])
            .map_err(|e| invalid(e.to_string()))?;
        let output = outputs
            .get("ai_logit")
            .ok_or_else(|| invalid("模型输出名称不匹配"))?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| invalid(e.to_string()))?;
        if **shape != [1, 1] || values.len() != 1 || !values[0].is_finite() {
            return Err(invalid("模型输出形状或分值无效"));
        }
        Ok(f64::from(values[0]))
    })();
    let (raw_score, error) = match result {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let reply = WorkerReply {
        protocol: 1,
        file_sha256: hash,
        model_sha256: MODEL_HASH.into(),
        preprocess: PREPROCESS.into(),
        raw_score,
        error,
    };
    serde_json::to_writer(std::io::stdout(), &reply).map_err(|e| invalid(e.to_string()))?;
    Ok(())
}
pub fn infer_in_worker(pack: &Path, bytes: &[u8], cancel: &Cancellation) -> Result<ModelResult> {
    // Integrity validation happens inside the killable worker too. Do not hash
    // multi-megabyte model/runtime files in an uninterruptible parent step.
    cancel.check()?;
    let executable = std::env::current_exe()?;
    infer_with_executable(&executable, pack, bytes, cancel, Duration::from_secs(5))
}
pub fn infer_with_executable(
    executable: &Path,
    pack: &Path,
    bytes: &[u8],
    cancel: &Cancellation,
    timeout: Duration,
) -> Result<ModelResult> {
    cancel.check()?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut command = Command::new(executable);
    command
        .arg("--ai-pixel-worker")
        .arg(pack)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("ORT_DYLIB_PATH")
        .env("OMP_NUM_THREADS", "2")
        .env("OMP_WAIT_POLICY", "PASSIVE");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let _job = match super::windows::Job::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid(format!("像素进程资源约束未建立：{error}")));
        }
    };

    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| invalid("检测进程输入不可用"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| invalid("检测进程输出不可用"))?;
    let bytes = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        input
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .and_then(|_| input.write_all(&bytes))
    });
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        output
            .take(MAX_REPORT + 1)
            .read_to_end(&mut out)
            .map(|_| out)
    });
    let start = Instant::now();
    let outcome = loop {
        if cancel.check().is_err() {
            let _ = child.kill();
            break Err(ToolError::Cancelled);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            break Err(invalid("像素检测超时（5 秒），已终止独立进程"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(invalid(format!("像素检测进程异常退出：{status}")))
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                break Err(e.into());
            }
        }
    };
    let _ = child.wait();
    let write_result = writer.join().map_err(|_| invalid("检测输入线程异常"));
    let output = reader.join().map_err(|_| invalid("检测输出线程异常"));
    outcome?;
    write_result??;
    let output = output??;
    if output.len() as u64 > MAX_REPORT {
        return Err(invalid("检测报告超过大小限制"));
    }
    let reply: WorkerReply =
        serde_json::from_slice(&output).map_err(|e| invalid(format!("检测报告无效：{e}")))?;
    if reply.protocol != 1
        || reply.file_sha256 != hash
        || reply.model_sha256 != MODEL_HASH
        || reply.preprocess != PREPROCESS
    {
        return Err(invalid("检测报告内容身份或组件版本不匹配"));
    }
    if let Some(e) = reply.error {
        return Err(invalid(e));
    }
    let raw_score = reply
        .raw_score
        .filter(|v| v.is_finite())
        .ok_or_else(|| invalid("检测分值无效"))?;
    cancel.check()?;
    Ok(ModelResult {
        fingerprint: fingerprint(),
        input_scope: "仅原始 RGB 中心 224×224 裁切，非整幅覆盖；实验未校准".into(),
        raw_score,
        threshold: RESEARCH_THRESHOLD,
        approved: false,
        high_signal: false,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crop_uses_python_ties_even() {
        assert_eq!(crop_offset(225), 0);
        assert_eq!(crop_offset(227), 2);
        assert_eq!(crop_offset(229), 2);
        assert_eq!(crop_offset(231), 4);
    }
    #[test]
    fn invalid_dimensions_reject_without_resize() {
        assert!(preprocess_rgb8(&image::RgbImage::new(223, 224)).is_err());
        assert!(preprocess_rgb8(&image::RgbImage::new(1121, 224)).is_err());
    }
    #[test]
    fn rgb_channels_normalized_planar() {
        let x = preprocess_rgb8(&image::RgbImage::from_pixel(
            224,
            224,
            image::Rgb([0, 128, 255]),
        ))
        .unwrap();
        assert_eq!(x.len(), 3 * 224 * 224);
        assert_eq!(x[0], -0.485 / 0.229);
        assert_eq!(x[224 * 224], (128.0 / 255.0 - 0.456) / 0.224);
        assert_eq!(x[2 * 224 * 224], (1.0 - 0.406) / 0.225);
    }
    #[test]
    fn arbitrary_pack_cannot_enable_model() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), br#"{"approved":true}"#).unwrap();
        assert!(validate_pack(dir.path()).is_err());
    }
}
