//! Resolve helpers from the current test binary's actual target/profile directory.
//! Never fall back to a stale debug binary when validating a release or --target build.
use std::path::{Path, PathBuf};

fn profile_dir_from(test_executable: &Path) -> Option<PathBuf> {
    let deps = test_executable.parent()?;
    if deps.file_name()? != "deps" {
        return None;
    }
    Some(deps.parent()?.to_path_buf())
}

pub fn profile_dir() -> PathBuf {
    let executable = std::env::current_exe().expect("locate the running Cargo test binary");
    profile_dir_from(&executable).unwrap_or_else(|| {
        panic!(
            "expected a Cargo test binary in a profile/deps directory: {}",
            executable.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_resolution_tracks_explicit_target_and_active_profile() {
        for (executable, expected) in [
            ("target/debug/deps/test.exe", "target/debug"),
            (
                "target/x86_64-pc-windows-msvc/debug/deps/test.exe",
                "target/x86_64-pc-windows-msvc/debug",
            ),
            (
                "target/x86_64-pc-windows-msvc/release/deps/test.exe",
                "target/x86_64-pc-windows-msvc/release",
            ),
            (
                "custom output/release/deps/test.exe",
                "custom output/release",
            ),
        ] {
            assert_eq!(
                profile_dir_from(Path::new(executable)),
                Some(PathBuf::from(expected))
            );
        }
    }

    #[test]
    fn helper_resolution_rejects_ambiguous_non_cargo_locations() {
        assert!(profile_dir_from(Path::new("test.exe")).is_none());
        assert!(profile_dir_from(Path::new("target/release/test.exe")).is_none());
    }
}
