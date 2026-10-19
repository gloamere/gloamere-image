//! Explicit, local-only component QA. Never chooses user images or discovers a pack.
use light_image_tool::{Cancellation, ai::model};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: ai_component_smoke <main-executable> <verified-pack-directory> <golden-fixtures-directory> <output-json>".into());
    }
    let executable = PathBuf::from(&args[0]);
    let pack = PathBuf::from(&args[1]);
    let fixtures = PathBuf::from(&args[2]);
    let output = PathBuf::from(&args[3]);
    let verified = model::validate_pack(&pack)?;
    let goldens: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(fixtures.join("golden-vectors.json"))?)?;
    let mut records = Vec::new();
    for golden in goldens {
        let filename = golden["image_file"].as_str().ok_or("missing image_file")?;
        if !filename.ends_with(".png") && !filename.ends_with(".jpg") {
            continue;
        }
        let path = fixtures.join(filename);
        let bytes = std::fs::read(&path)?;
        let expected = golden["reference_fp64_head_logit"]
            .as_f64()
            .ok_or("missing reference")?;
        let tensor = model::prepare_tensor(
            &bytes,
            &verified
                .directory
                .join(&verified.manifest.jpeg_decoder.name),
        )?;
        let reference =
            std::fs::read(fixtures.join(golden["tensor_file"].as_str().ok_or("missing tensor")?))?;
        let actual: Vec<u8> = tensor.iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(actual, reference, "preprocess mismatch {filename}");
        let start = Instant::now();
        let result = model::infer_with_executable(
            &executable,
            &pack,
            &bytes,
            &Cancellation::default(),
            Duration::from_secs(5),
        )?;
        let error = (result.raw_score - expected).abs();
        assert!(error <= 1e-4, "{filename}: {error}");
        assert!(!result.approved && !result.high_signal);
        records.push(serde_json::json!({"fixture":filename,"raw_logit":result.raw_score,"reference":expected,"max_abs_error":error,"whole_job_ms":start.elapsed().as_secs_f64()*1000.0,"preprocess_bit_exact":true,"quality_approval":false}));
    }
    let first = records
        .first()
        .and_then(|r| r["fixture"].as_str())
        .ok_or("no admitted goldens")?;
    let bytes = std::fs::read(fixtures.join(first))?;
    let cancel = Cancellation::default();
    let token = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        token.cancel();
    });
    let started = Instant::now();
    let result =
        model::infer_with_executable(&executable, &pack, &bytes, &cancel, Duration::from_secs(5));
    canceller.join().unwrap();
    assert!(matches!(
        result,
        Err(light_image_tool::ToolError::Cancelled)
    ));
    let cancel_ms = started.elapsed().as_secs_f64() * 1000.;
    assert!(cancel_ms < 1000.0, "cancel latency {cancel_ms}");
    let timeout_start = Instant::now();
    let timeout = model::infer_with_executable(
        &executable,
        &pack,
        &bytes,
        &Cancellation::default(),
        Duration::from_millis(1),
    );
    assert!(timeout.is_err());
    let timeout_reaped_ms = timeout_start.elapsed().as_secs_f64() * 1000.;
    let recovered = model::infer_with_executable(
        &executable,
        &pack,
        &bytes,
        &Cancellation::default(),
        Duration::from_secs(5),
    )?;
    assert!(!recovered.approved);
    let report = serde_json::json!({"scope":"fixed PNG/JPEG pipeline parity/process cancellation only; no quality or native Windows acceptance","vectors":records,"cancellation_ms":cancel_ms,"timeout_reaped_ms":timeout_reaped_ms,"recovered_after_timeout":true});
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", output.display());
    Ok(())
}
