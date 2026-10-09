//! Shared, dependency-free Windows SDK resource integration for both executables.
//! See docs/branding-icons.md for the native-build/source-check boundary.

use std::{env, fs, path::Path, process::Command};

#[derive(Clone, Copy)]
pub struct Product {
    pub binary: &'static str,
    pub description: &'static str,
}

pub const MAIN: Product = Product {
    binary: "gloamere-image",
    description: "gloamere-image",
};
pub const WORKER: Product = Product {
    binary: "gloamere-image-worker",
    description: "gloamere-image metadata and provenance worker",
};
const CHECK_ONLY: &str = "GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY";

pub fn build(project_root: &Path, product: Product) {
    println!("cargo:rerun-if-changed=build.rs");
    for file in [
        "build-support/windows_resources.rs",
        "assets/gloamere-image.ico",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            project_root.join(file).display()
        );
    }
    for variable in [
        CHECK_ONLY,
        "RC",
        "PATH",
        "WindowsSdkDir",
        "WindowsSDKVersion",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let target = required("TARGET");
    if !target.contains("-windows-") {
        return;
    }
    assert!(
        target.ends_with("-windows-msvc"),
        "gloamere-image Windows binaries require the supported MSVC target and Windows SDK resources; target was {target}"
    );
    println!("cargo:rustc-link-arg-bin={}=/Brepro", product.binary);
    let host = required("HOST");
    let profile = required("PROFILE");
    let check_only = env::var(CHECK_ONLY).ok();
    let skip = check_only_policy(&host, &target, &profile, check_only.as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let out = std::path::PathBuf::from(required("OUT_DIR"));
    if skip {
        println!(
            "cargo:warning=Windows resources are NOT compiled: explicit non-Windows debug source-check mode; this does not validate a native EXE."
        );
        // Cargo check ignores linker arguments. A real link must fail rather than
        // producing an unbranded EXE. This sentinel is intentionally never created.
        let sentinel = out.join("CHECK_ONLY_NO_WINDOWS_RESOURCES_DO_NOT_LINK.res");
        assert!(
            !sentinel.exists(),
            "check-only link sentinel must not exist"
        );
        println!(
            "cargo:rustc-link-arg-bin={}={}",
            product.binary,
            sentinel.display()
        );
        return;
    }
    assert!(
        host.contains("-windows-"),
        "Native Windows/MSVC resource compilation requires the Windows SDK on Windows. For source validation only, run debug cargo check with {CHECK_ONLY}=1; release packaging must use the native Windows SDK build."
    );
    let icon = project_root.join("assets/gloamere-image.ico");
    let bytes = fs::read(&icon)
        .unwrap_or_else(|error| panic!("Required app icon {}: {error}", icon.display()));
    assert!(
        bytes.starts_with(&[0, 0, 1, 0, 9, 0]),
        "App icon must contain all nine required ICO frames"
    );
    fs::write(out.join("gloamere-image.ico"), bytes).expect("copy app icon to OUT_DIR");
    let resource_name = format!("{}.rc", product.binary);
    let output_name = format!("{}.res", product.binary);
    let version = required("CARGO_PKG_VERSION");
    let script = resource_script(product, &version, profile == "debug")
        .unwrap_or_else(|message| panic!("Invalid Windows resource metadata: {message}"));
    fs::write(out.join(&resource_name), script).expect("write Windows resource script");
    let resource = out.join(&output_name);
    // Never accept a stale resource left by an earlier build.
    if resource.exists() {
        fs::remove_file(&resource).expect("remove previous resource output");
    }
    let compiler = env::var_os("RC").unwrap_or_else(|| "rc.exe".into());
    let output = Command::new(&compiler)
        .current_dir(&out)
        .args(["/nologo", "/fo", &output_name, &resource_name])
        .output()
        .unwrap_or_else(|error| panic!("Cannot run Windows SDK resource compiler {compiler:?}: {error}. Initialize the VS Build Tools + Windows SDK environment, or set RC to the official SDK rc.exe path. Branding resources are mandatory."));
    assert!(
        output.status.success(),
        "Windows SDK resource compilation failed ({:?}).\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::metadata(&resource).is_ok_and(|metadata| metadata.len() > 32),
        "Windows SDK did not produce a nonempty .res; refusing to link without app resources"
    );
    println!(
        "cargo:rustc-link-arg-bin={}={}",
        product.binary,
        resource.display()
    );
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("Cargo must provide {name}"))
}

fn check_only_policy(
    host: &str,
    target: &str,
    profile: &str,
    flag: Option<&str>,
) -> Result<bool, String> {
    match flag {
        None | Some("") | Some("0") => Ok(false),
        Some("1")
            if !host.contains("-windows-")
                && target.ends_with("-windows-msvc")
                && profile == "debug" =>
        {
            Ok(true)
        }
        Some("1") => Err(format!(
            "{CHECK_ONLY}=1 is only permitted for a non-Windows host's debug Windows/MSVC source check; it is forbidden for native Windows and release builds"
        )),
        Some(_) => Err(format!("{CHECK_ONLY} must be 0 or 1")),
    }
}

fn fixed_version(version: &str) -> Result<String, String> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let parts = core
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "version components must be integers in 0..=65535".to_owned())?;
    if parts.len() != 3 {
        return Err("version must have major.minor.patch components".to_owned());
    }
    Ok(format!("{},{},{},0", parts[0], parts[1], parts[2]))
}

fn resource_script(product: Product, version: &str, debug: bool) -> Result<String, String> {
    if !version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        return Err("version contains characters unsafe for an RC string".to_owned());
    }
    let fixed = fixed_version(version)?;
    let binary = product.binary;
    let description = product.description;
    let flags = if debug { "0x1L" } else { "0x0L" };
    // Numeric constants avoid reliance on INCLUDE or windows.h while rc.exe still
    // comes from the official SDK. English/Unicode metadata is locale-independent.
    Ok(format!(
        r#"// Generated by gloamere-image. Do not edit build outputs.
LANGUAGE 9, 1
1 ICON "gloamere-image.ico"
1 VERSIONINFO
FILEVERSION {fixed}
PRODUCTVERSION {fixed}
FILEFLAGSMASK 0x3fL
FILEFLAGS {flags}
FILEOS 0x00040004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "gloamere-image contributors\0"
            VALUE "FileDescription", "{description}\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "{binary}\0"
            VALUE "OriginalFilename", "{binary}.exe\0"
            VALUE "ProductName", "gloamere-image\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_and_worker_have_exact_identity_and_shared_product() {
        for (product, filename) in [
            (MAIN, "gloamere-image.exe"),
            (WORKER, "gloamere-image-worker.exe"),
        ] {
            let rc = resource_script(product, "0.4.0", false).unwrap();
            assert!(rc.contains("1 ICON \"gloamere-image.ico\""));
            assert!(rc.contains("1 VERSIONINFO"));
            assert!(rc.contains("FILEVERSION 0,4,0,0"));
            assert!(rc.contains("PRODUCTVERSION 0,4,0,0"));
            assert!(rc.contains("VALUE \"ProductName\", \"gloamere-image\\0\""));
            assert!(rc.contains(&format!("VALUE \"OriginalFilename\", \"{filename}\\0\"")));
            assert!(rc.contains("FILEFLAGS 0x0L"));
            assert!(!rc.contains("light-image-tool"));
            assert!(!rc.contains("light-provenance-worker"));
        }
        assert!(
            resource_script(MAIN, "0.4.0", true)
                .unwrap()
                .contains("FILEFLAGS 0x1L")
        );
    }

    #[test]
    fn fixed_version_is_bounded_and_rc_strings_are_safe() {
        assert_eq!(fixed_version("0.4.0").unwrap(), "0,4,0,0");
        assert_eq!(fixed_version("1.2.3-beta.1").unwrap(), "1,2,3,0");
        for version in ["", "1.2", "1.2.3.4", "65536.0.0", "a.b.c", "0.4.0\"\nBAD"] {
            assert!(resource_script(MAIN, version, false).is_err(), "{version}");
        }
    }

    #[test]
    fn override_cannot_make_native_or_release_builds_silently_omit_resources() {
        let linux = "x86_64-unknown-linux-gnu";
        let windows = "x86_64-pc-windows-msvc";
        assert!(check_only_policy(linux, windows, "debug", Some("1")).unwrap());
        assert!(!check_only_policy(linux, windows, "debug", None).unwrap());
        assert!(!check_only_policy(windows, windows, "release", Some("0")).unwrap());
        assert!(check_only_policy(linux, windows, "release", Some("1")).is_err());
        assert!(check_only_policy(windows, windows, "debug", Some("1")).is_err());
        assert!(check_only_policy(linux, windows, "debug", Some("yes")).is_err());
        assert!(check_only_policy(linux, "x86_64-pc-windows-gnu", "debug", Some("1")).is_err());
    }
}
