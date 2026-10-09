mod support;
use light_image_tool::provenance::CheckState;
use light_image_tool::{
    Cancellation,
    provenance::{
        EvidenceCache, EvidenceReport, EvidenceState, POLICY_VERSION, TRUST_SNAPSHOT,
        inspect_snapshot,
    },
};
use std::path::PathBuf;

#[test]
fn reports_are_keyed_by_all_file_bytes_policy_and_trust() {
    let mut cache = EvidenceCache::default();
    let mut report = EvidenceReport::incomplete("first-hash".into(), "fixture");
    report.state = EvidenceState::NoCredential;
    cache.insert(report.clone());
    assert!(cache.get("second-hash").is_none());
    assert!(cache.get("first-hash").is_some());
    report.policy_version = "old-policy".into();
    cache.insert(report.clone());
    report.file_sha256 = "only-old".into();
    cache.insert(report.clone());
    assert!(cache.get("only-old").is_none());
    report.policy_version = POLICY_VERSION.into();
    report.trust_snapshot = "old-trust".into();
    cache.insert(report);
    assert!(cache.get("only-old").is_none());
    assert!(cache.bytes() < 1024 * 1024);
    assert_eq!(
        cache.get("first-hash").unwrap().trust_snapshot,
        TRUST_SNAPSHOT
    );
}

#[test]
fn report_cache_is_bounded_by_serialized_bytes() {
    let mut cache = EvidenceCache::default();
    for i in 0..2000 {
        let mut report = EvidenceReport::incomplete(format!("hash-{i}"), "x".repeat(1800));
        report.state = EvidenceState::NoCredential;
        cache.insert(report);
        assert!(cache.bytes() <= 1024 * 1024);
    }
    assert!(cache.get("hash-0").is_none());
    assert!(cache.get("hash-1999").is_some());
}

fn worker() -> PathBuf {
    support::profile_dir().join("gloamere-image-worker.exe")
}

#[test]
#[ignore = "requires built workspace worker; run explicitly after cargo build --workspace"]
fn real_windows_job_worker_reads_no_credential_fixture() {
    let mut bytes = Vec::new();
    let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 128]));
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    let report = inspect_snapshot(bytes, &worker(), &Cancellation::default()).unwrap();
    assert_eq!(report.state, EvidenceState::NoCredential);
    assert_eq!(report.blocked_http_attempts, 0);
}

#[test]
#[ignore = "requires built workspace worker; run explicitly after cargo build --workspace"]
fn file_mutation_cannot_reuse_loaded_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    image::RgbaImage::from_pixel(4, 3, image::Rgba([0, 0, 0, 255]))
        .save(&path)
        .unwrap();
    let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
    image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 0, 0, 255]))
        .save(&path)
        .unwrap();
    let result = light_image_tool::provenance::inspect(
        &path,
        &loaded.source_sha256,
        &Cancellation::default(),
    );
    assert!(result.is_err());
}

#[test]
#[ignore = "requires built workspace worker; scripts/verify.ps1 includes ignored integration tests"]
fn real_windows_worker_classifies_signed_tampered_remote_and_ocsp_fixtures() {
    for (name, bytes) in [
        (
            "signed",
            include_bytes!("fixtures/provenance/C.jpg").as_slice(),
        ),
        (
            "ocsp",
            include_bytes!("fixtures/provenance/ocsp.jpg").as_slice(),
        ),
        (
            "remote",
            include_bytes!("fixtures/provenance/libpng-test_with_url.png").as_slice(),
        ),
    ] {
        let report = inspect_snapshot(bytes.to_vec(), &worker(), &Cancellation::default()).unwrap();
        assert_eq!(report.blocked_http_attempts, 0, "{name}");
        assert_eq!(report.trust_snapshot, "none-v1");
        if name == "remote" {
            assert_eq!(report.state, EvidenceState::Incomplete);
        } else {
            assert_eq!(report.signature, CheckState::Passed);
            assert_eq!(report.asset_binding, CheckState::Passed);
            assert!(!report.signed_declarations.is_empty());
        }
    }
    let mut tampered = include_bytes!("fixtures/provenance/C.jpg").to_vec();
    let index = tampered.len() - 100;
    tampered[index] ^= 1;
    let report = inspect_snapshot(tampered, &worker(), &Cancellation::default()).unwrap();
    assert_eq!(report.asset_binding, CheckState::Failed);
    assert_eq!(report.signature, CheckState::Passed);
    let bad_signature = inspect_snapshot(
        include_bytes!("fixtures/provenance/E-sig-CA.jpg").to_vec(),
        &worker(),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(bad_signature.signature, CheckState::Failed);
    assert_eq!(bad_signature.asset_binding, CheckState::Passed);
}

#[test]
#[ignore = "requires built workspace worker; scripts/verify.ps1 includes ignored integration tests"]
fn actual_worker_does_not_connect_to_remote_manifest_loopback_and_does_not_authenticate_prompt() {
    use image::ImageDecoder;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let bytes = include_bytes!("fixtures/provenance/libpng-test_with_url.png");
    let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes)).unwrap();
    let xmp = String::from_utf8(decoder.xmp_metadata().unwrap().unwrap()).unwrap();
    assert!(xmp.contains("http://localhost:5000/libpng-test.c2pa"));
    let replaced = xmp.replace(
        "http://localhost:5000/libpng-test.c2pa",
        &format!("http://{}/remote.c2pa", listener.local_addr().unwrap()),
    );
    let pixels = image::load_from_memory(bytes).unwrap().into_rgba8();
    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, pixels.width(), pixels.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_itxt_chunk("XML:com.adobe.xmp".into(), replaced)
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels.as_raw()).unwrap();
    writer.finish().unwrap();
    let report = inspect_snapshot(encoded, &worker(), &Cancellation::default()).unwrap();
    assert_eq!(report.state, EvidenceState::Incomplete);
    assert_eq!(report.blocked_http_attempts, 0);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let mut prompt_png = Vec::new();
    let mut encoder = png::Encoder::new(&mut prompt_png, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk(
            "parameters".into(),
            "AI prompt fixture, not authentication".into(),
        )
        .unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[1, 2, 3, 255]).unwrap();
    writer.finish().unwrap();
    let report = inspect_snapshot(prompt_png, &worker(), &Cancellation::default()).unwrap();
    assert_eq!(report.state, EvidenceState::NoCredential);
    assert_eq!(report.signature, CheckState::NotChecked);
    assert!(!report.unsigned_claims.is_empty());
}
