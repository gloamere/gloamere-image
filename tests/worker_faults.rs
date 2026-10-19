mod support;
use light_image_tool::{Cancellation, ToolError, provenance::inspect_snapshot};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires cargo build --workspace --examples; actual Windows process fault injection"]
fn actual_worker_faults_are_errors_cancel_timeout_and_recover() {
    let target = support::profile_dir();
    let dir = tempfile::tempdir().unwrap();
    let fixture = include_bytes!("fixtures/provenance/C.jpg");
    let baseline = light_image_tool::memory::in_use();
    let mut results = Vec::new();
    for mode in [
        "malformed",
        "oversize",
        "crash",
        "identity",
        "memory",
        "hang",
        "blocked-input",
    ] {
        let helper = dir.path().join(format!("fault-{mode}.exe"));
        std::fs::copy(target.join("examples/fault_worker.exe"), &helper).unwrap();
        let cancel = Cancellation::default();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        if mode == "blocked-input" {
            let token = cancel.clone();
            let marker = helper.with_extension("ready");
            std::thread::spawn(move || {
                let started = Instant::now();
                while !marker.is_file() && started.elapsed() < Duration::from_secs(6) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let entered = marker.is_file();
                std::thread::sleep(Duration::from_millis(150));
                let requested = Instant::now();
                token.cancel();
                let _ = ready_tx.send((entered, requested));
            });
        }
        let bytes = if mode == "blocked-input" {
            // Exceeds the measured Windows anonymous pipe buffer (4 KiB) by 512x.
            // Keep this an IPC-cancellation test, rather than a 32 MiB Debug hash benchmark.
            vec![7; 2 * 1024 * 1024]
        } else {
            fixture.to_vec()
        };
        let payload_bytes = bytes.len();
        let mut peer_ready = None;
        let mut cancellation_response_ms = None;
        let start = Instant::now();
        let result = inspect_snapshot(bytes, &helper, &cancel);
        let elapsed = start.elapsed();
        let error = result.expect_err(mode);
        if mode == "blocked-input" {
            assert!(matches!(error, ToolError::Cancelled));
            let (entered, requested) = ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let response = requested.elapsed();
            peer_ready = Some(entered);
            cancellation_response_ms = Some(response.as_millis());
            assert!(
                entered,
                "cancellation must exercise an actual blocked IPC peer"
            );
            assert!(response < Duration::from_millis(750));
            assert!(elapsed < Duration::from_secs(6));
        } else if mode == "hang" {
            assert!(elapsed >= Duration::from_secs(20) && elapsed < Duration::from_secs(23));
        } else {
            assert!(elapsed < Duration::from_secs(5), "{mode} {elapsed:?}");
        }
        let release_start = Instant::now();
        while light_image_tool::memory::in_use() != baseline
            && release_start.elapsed() < Duration::from_secs(2)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            light_image_tool::memory::in_use(),
            baseline,
            "IPC snapshot retained after {mode}"
        );
        results.push(serde_json::json!({"mode":mode,"payload_bytes":payload_bytes,"peer_ready":peer_ready,"cancellation_response_ms":cancellation_response_ms,"elapsed_ms":elapsed.as_millis(),"error":error.to_string(),"snapshot_budget_released":true}));
    }
    let report = inspect_snapshot(
        fixture.to_vec(),
        &target.join("gloamere-image-worker.exe"),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(
        report.signature,
        light_image_tool::provenance::CheckState::Passed
    );
    let report = serde_json::json!({"faults":results,"real_sdk_recovered":true});
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-output/checkpoint-2/worker-fault-results.json");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
