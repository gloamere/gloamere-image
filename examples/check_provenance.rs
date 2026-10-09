use light_image_tool::{Cancellation, provenance};
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(args.len() >= 3, "worker output-directory fixture...");
    let worker = std::path::PathBuf::from(&args[0]);
    let output = std::path::PathBuf::from(&args[1]);
    std::fs::create_dir_all(&output).unwrap();
    for path in &args[2..] {
        let path = std::path::PathBuf::from(path);
        let bytes = std::fs::read(&path).unwrap();
        match provenance::inspect_snapshot(bytes, &worker, &Cancellation::default()) {
            Ok(report) => {
                let name = path.file_name().unwrap().to_string_lossy();
                let report_path = output.join(format!("{name}.json"));
                std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
                println!(
                    "{name}: {:?} signature={:?} binding={:?} blocked_http={}",
                    report.state,
                    report.signature,
                    report.asset_binding,
                    report.blocked_http_attempts
                );
            }
            Err(error) => {
                println!("{}: ERROR {error}", path.display());
                std::process::exit(1);
            }
        }
    }
}
