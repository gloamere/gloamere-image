//! Test-only IPC peer: never shipped with the product.
use std::io::{Read, Write};
fn main() {
    let path = std::env::current_exe().unwrap();
    let mode = path.file_stem().unwrap().to_string_lossy();
    if mode.contains("blocked-input") {
        std::fs::write(
            path.with_extension("ready"),
            b"own worker entered blocked input mode",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    let mut size = [0; 8];
    std::io::stdin().read_exact(&mut size).unwrap();
    let mut bytes = vec![0; u64::from_le_bytes(size) as usize];
    std::io::stdin().read_exact(&mut bytes).unwrap();
    if mode.contains("crash") {
        std::process::exit(7);
    } else if mode.contains("oversize") {
        std::io::stdout()
            .write_all(&vec![b'x'; 64 * 1024 + 1])
            .unwrap();
    } else if mode.contains("hang") {
        std::thread::sleep(std::time::Duration::from_secs(120));
    } else if mode.contains("memory") {
        let mut allocations = Vec::new();
        for _ in 0..80 {
            allocations.push(vec![1u8; 8 * 1024 * 1024]);
        }
        std::hint::black_box(allocations);
        std::process::exit(8);
    } else if mode.contains("identity") {
        let report = light_image_tool::provenance::EvidenceReport::incomplete(
            "wrong-file-hash".into(),
            "test-only forged identity",
        );
        serde_json::to_writer(std::io::stdout(), &report).unwrap();
    } else {
        std::io::stdout().write_all(b"{ malformed JSON").unwrap();
    }
}
