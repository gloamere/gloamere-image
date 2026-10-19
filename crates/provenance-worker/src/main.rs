#[path = "../../../src/provenance/report.rs"]
pub mod report;
use c2pa::{
    Context, Reader,
    http::{
        HttpResolverError, SyncHttpResolver,
        http::{Request, Response},
    },
    settings::Settings,
};
use image::ImageDecoder;
use report::*;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Cursor, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone, Default)]
struct DenyNetwork(Arc<AtomicU64>);
impl SyncHttpResolver for DenyNetwork {
    fn http_resolve(
        &self,
        _request: Request<Vec<u8>>,
    ) -> std::result::Result<Response<Box<dyn Read>>, HttpResolverError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(HttpResolverError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "offline policy denies all HTTP",
        )))
    }
}
fn offline_context(deny: DenyNetwork) -> c2pa::Result<Context> {
    let settings = Settings::new()
        .with_value("verify.verify_after_reading", true)?
        .with_value("verify.remote_manifest_fetch", false)?
        .with_value("verify.ocsp_fetch", false)?
        .with_value("verify.verify_trust", true)?
        .with_value("verify.verify_timestamp_trust", true)?;
    Ok(Context::new().with_settings(settings)?.with_resolver(deny))
}

fn bounded_value(value: impl AsRef<str>) -> String {
    value.as_ref().chars().take(512).collect()
}
fn push_claim(report: &mut EvidenceReport, label: &str, value: &str) {
    if report.unsigned_claims.len() < 8 && !value.trim().is_empty() {
        report.unsigned_claims.push(MetadataField {
            label: bounded_value(label),
            value: bounded_value(value),
        });
    }
}
fn metadata(bytes: &[u8], format: image::ImageFormat, report: &mut EvidenceReport) {
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
        for tag in [
            exif::Tag::Make,
            exif::Tag::Model,
            exif::Tag::DateTimeOriginal,
        ] {
            if let Some(field) = exif.get_field(tag, exif::In::PRIMARY) {
                report.camera_exif.push(MetadataField {
                    label: tag.to_string(),
                    value: bounded_value(field.display_value().with_unit(&exif).to_string()),
                });
            }
        }
        if let Some(field) = exif.get_field(exif::Tag::Software, exif::In::PRIMARY) {
            let value = field.display_value().to_string();
            let lower = value.to_ascii_lowercase();
            if [
                "stable diffusion",
                "midjourney",
                "comfyui",
                "dall-e",
                "flux",
            ]
            .iter()
            .any(|s| lower.contains(s))
            {
                push_claim(report, "EXIF Software（未签名工具声明）", &value);
            }
        }
    }
    if format == image::ImageFormat::Png {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_limits(png::Limits {
            bytes: 16 * 1024 * 1024,
        });
        if let Ok(reader) = decoder.read_info() {
            let info = reader.info();
            for field in &info.uncompressed_latin1_text {
                if ["parameters", "prompt", "negative_prompt", "software"]
                    .contains(&field.keyword.to_ascii_lowercase().as_str())
                {
                    push_claim(
                        report,
                        &format!("PNG {}（未签名）", field.keyword),
                        &field.text,
                    );
                }
            }
            for field in &info.utf8_text {
                if ["parameters", "prompt", "negative_prompt", "software"]
                    .contains(&field.keyword.to_ascii_lowercase().as_str())
                    && let Ok(text) = field.get_text()
                {
                    push_claim(report, &format!("PNG {}（未签名）", field.keyword), &text);
                }
            }
            if !info.compressed_latin1_text.is_empty() {
                report
                    .offline_limitations
                    .push("未解析压缩 PNG 文本声明".into());
            }
        }
    }
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    if let Ok(mut decoder) = reader.into_decoder()
        && let Ok(Some(xmp)) = decoder.xmp_metadata()
    {
        if xmp.len() > 1024 * 1024 {
            report.offline_limitations.push("XMP 超过读取限制".into());
            return;
        }
        let mut xml = quick_xml::Reader::from_reader(xmp.as_slice());
        xml.config_mut().trim_text(true);
        let mut current = String::new();
        for _ in 0..10_000 {
            match xml.read_event() {
                Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                    current = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                    for attr in e.attributes().flatten() {
                        let name = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        if [
                            "prompt",
                            "parameters",
                            "generatingtool",
                            "digitalsourcetype",
                        ]
                        .iter()
                        .any(|s| name.ends_with(s))
                        {
                            push_claim(
                                report,
                                &format!("XMP {name}（未签名声明）"),
                                &String::from_utf8_lossy(&attr.value),
                            );
                        }
                    }
                }
                Ok(quick_xml::events::Event::Text(e)) => {
                    if [
                        "prompt",
                        "parameters",
                        "generatingtool",
                        "digitalsourcetype",
                    ]
                    .iter()
                    .any(|s| current.ends_with(s))
                    {
                        push_claim(
                            report,
                            &format!("XMP {current}（未签名声明）"),
                            &String::from_utf8_lossy(e.as_ref()),
                        );
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => {
                    report.offline_limitations.push("XMP 解析不完整".into());
                    break;
                }
                _ => {}
            }
        }
    }
}

fn classify_codes(report: &mut EvidenceReport, success: &[String], failure: &[String]) {
    report.signature = if failure.iter().any(|s| s.starts_with("claimSignature.")) {
        CheckState::Failed
    } else if success.iter().any(|s| s == "claimSignature.validated") {
        CheckState::Passed
    } else {
        CheckState::NotChecked
    };
    report.asset_binding = if failure.iter().any(|s| {
        s.starts_with("assertion.dataHash.")
            || s.starts_with("assertion.bmffHash.")
            || s.starts_with("assertion.boxesHash.")
    }) {
        CheckState::Failed
    } else if success.iter().any(|s| {
        s == "assertion.dataHash.match"
            || s == "assertion.bmffHash.match"
            || s == "assertion.boxesHash.match"
    }) {
        CheckState::Passed
    } else {
        CheckState::NotChecked
    };
}

fn inspect(bytes: &[u8]) -> EvidenceReport {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut report = EvidenceReport::incomplete(hash, "核验未完成");
    let format = match image::guess_format(bytes) {
        Ok(f @ (image::ImageFormat::Jpeg | image::ImageFormat::Png)) => f,
        _ => {
            report.details = "不支持的图像格式".into();
            return report;
        }
    };
    metadata(bytes, format, &mut report);
    let deny = DenyNetwork::default();
    let context = match offline_context(deny.clone()) {
        Ok(c) => c,
        Err(e) => {
            report.state = EvidenceState::Error;
            report.details = bounded_value(format!("离线设置失败：{e}"));
            return report;
        }
    };
    let mime = if format == image::ImageFormat::Jpeg {
        "image/jpeg"
    } else {
        "image/png"
    };
    match Reader::from_context(context).with_stream(mime, Cursor::new(bytes)) {
        Ok(reader) => {
            if let Some(manifest) = reader.active_manifest() {
                for (label, value) in [
                    ("凭证标题", manifest.title()),
                    ("声明工具", manifest.claim_generator()),
                    (
                        "证书签发者",
                        manifest.signature_info().and_then(|s| s.issuer.as_deref()),
                    ),
                ] {
                    if let Some(value) = value {
                        report.signed_declarations.push(MetadataField {
                            label: label.into(),
                            value: bounded_value(value),
                        });
                    }
                }
            }
            report.state = if reader.active_manifest().is_some() {
                EvidenceState::Signed
            } else {
                EvidenceState::Incomplete
            };
            let mut success = Vec::new();
            let mut failure = Vec::new();
            if let Some(codes) = reader
                .validation_results()
                .and_then(|v| v.active_manifest())
            {
                success = codes.success.iter().map(|s| s.code().to_string()).collect();
                failure = codes.failure.iter().map(|s| s.code().to_string()).collect();
                report.status_codes = success
                    .iter()
                    .chain(failure.iter())
                    .chain(
                        codes
                            .informational
                            .iter()
                            .map(|s| s.code().to_string())
                            .collect::<Vec<_>>()
                            .iter(),
                    )
                    .take(40)
                    .cloned()
                    .collect();
            } else if let Some(status) = reader.validation_status() {
                failure = status.iter().map(|s| s.code().to_string()).collect();
                report.status_codes = failure.iter().take(40).cloned().collect();
            }
            classify_codes(&mut report, &success, &failure);
            report.details = format!(
                "SDK {:?}；签名、资产绑定与信任结果需分别解释",
                reader.validation_state()
            );
            // Never promote trust with an empty snapshot, even if a future SDK behavior changes.
            report.trust = "未配置信任快照；不能确认签发者受信任".into();
            if report.signature == CheckState::Failed || report.asset_binding == CheckState::Failed
            {
                report.details = "发现签名或资产绑定失败；不是有效来源凭证".into();
            }
        }
        Err(c2pa::Error::JumbfNotFound) => {
            report.state = EvidenceState::NoCredential;
            report.details = "没有发现嵌入的 C2PA 凭证；不代表图片真实或非 AI".into();
        }
        Err(c2pa::Error::RemoteManifestUrl(_)) | Err(c2pa::Error::RemoteManifestFetch(_)) => {
            report.state = EvidenceState::Incomplete;
            report.details = "凭证需要远端 manifest，离线模式未获取".into();
        }
        Err(e) => {
            report.state = EvidenceState::Error;
            report.details = bounded_value(format!("SDK 核验错误：{e}"));
        }
    }
    report.blocked_http_attempts = deny.0.load(Ordering::Relaxed);
    report
}

fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut input = io::stdin().lock();
    let mut length = [0u8; 8];
    input.read_exact(&mut length)?;
    let len = u64::from_le_bytes(length);
    if len == 0 || len > MAX_INPUT_BYTES {
        return Err("input length exceeds worker limit".into());
    }
    let mut bytes = Vec::new();
    input.by_ref().take(len).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != len {
        return Err("truncated worker input".into());
    }
    let result = inspect(&bytes);
    let json = serde_json::to_vec(&result)?;
    if json.len() > MAX_REPORT_BYTES {
        return Err("report size exceeds worker limit".into());
    }
    io::stdout().lock().write_all(&json)?;
    Ok(())
}
fn installed_session_guard() -> io::Result<Option<std::fs::File>> {
    let current = std::env::current_exe()?;
    if current
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str())
        != Some("runtime")
    {
        return Ok(None);
    }
    #[cfg(windows)]
    let root = {
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
        };
        let mut text = std::ptr::null_mut();
        let status = unsafe {
            SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut text)
        };
        if status < 0 || text.is_null() {
            return Err(io::Error::other("cannot resolve LocalAppData"));
        }
        let mut length = 0;
        unsafe {
            while length < 32768 && *text.add(length) != 0 {
                length += 1;
            }
        }
        let path = if length < 32768 {
            Some(
                std::path::PathBuf::from(String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(text, length)
                }))
                .join("LightImageTool/updater-state"),
            )
        } else {
            None
        };
        unsafe {
            CoTaskMemFree(text.cast());
        }
        path
    };
    #[cfg(not(windows))]
    let root = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|p| p.join("Library/Application Support/LightImageTool/updater-state"));
    let root = root.ok_or_else(|| io::Error::other("user update state unavailable"))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(root.join("sessions.lock"))?;
    file.try_lock_shared()
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(Some(file))
}
fn main() {
    let _session = match installed_session_guard() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("worker update lock: {error}");
            std::process::exit(2);
        }
    };
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == "--health-protocol")
    {
        if offline_context(DenyNetwork::default()).is_err() {
            std::process::exit(2);
        }
        println!(
            "{}",
            serde_json::json!({"protocol":1,"version":env!("CARGO_PKG_VERSION"),"offline":true})
        );
        return;
    }
    if let Err(e) = run() {
        eprintln!("provenance worker: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_explicitly_disables_fetches() {
        let context = offline_context(DenyNetwork::default()).unwrap();
        assert!(!context.settings().verify.remote_manifest_fetch);
        assert!(!context.settings().verify.ocsp_fetch);
    }
    #[test]
    fn resolver_denies_even_loopback_without_connecting() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let deny = DenyNetwork::default();
        let request = Request::builder()
            .uri(format!("http://{}/ocsp", listener.local_addr().unwrap()))
            .body(Vec::new())
            .unwrap();
        assert!(deny.http_resolve(request).is_err());
        assert_eq!(deny.0.load(Ordering::Relaxed), 1);
        assert!(listener.accept().is_err());
    }
    #[test]
    fn signature_and_binding_are_not_inferred_from_prompt_or_total_state() {
        let mut r = EvidenceReport::incomplete("hash".into(), "test");
        classify_codes(
            &mut r,
            &["claimSignature.validated".into()],
            &["assertion.dataHash.mismatch".into()],
        );
        assert_eq!(r.signature, CheckState::Passed);
        assert_eq!(r.asset_binding, CheckState::Failed);
        assert_eq!(r.trust_snapshot, "none-v1");
    }
    #[test]
    fn actual_sdk_remote_and_ocsp_fixtures_use_no_http_resolver() {
        for (name, bytes) in [
            (
                "remote",
                include_bytes!("../../../tests/fixtures/provenance/libpng-test_with_url.png")
                    .as_slice(),
            ),
            (
                "ocsp",
                include_bytes!("../../../tests/fixtures/provenance/ocsp.jpg").as_slice(),
            ),
        ] {
            let report = inspect(bytes);
            assert_eq!(report.blocked_http_attempts, 0, "{name}");
            if name == "remote" {
                assert_eq!(report.state, EvidenceState::Incomplete);
            } else {
                assert_eq!(report.signature, CheckState::Passed);
                assert_eq!(report.asset_binding, CheckState::Passed);
            }
        }
    }
}
