#[path = "../../build-support/windows_resources.rs"]
#[allow(dead_code)]
mod windows_resources;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    windows_resources::build(&root, windows_resources::WORKER);
}
