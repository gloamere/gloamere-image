#[path = "build-support/windows_resources.rs"]
#[allow(dead_code)]
mod windows_resources;

fn main() {
    windows_resources::build(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        windows_resources::UI,
    );
    windows_resources::build(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        windows_resources::MAIN,
    );
}
