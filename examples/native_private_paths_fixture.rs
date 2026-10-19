//! Nonshipping native fixture; no real installer, registry or permission setters.
#[cfg(windows)]
use light_image_tool::updater::{Result, UpdateError, storage};
#[cfg(windows)]
#[path = "../src/updater/private_paths.rs"]
mod private_paths;
#[cfg(windows)]
mod fixture {
    use crate::private_paths;
    use light_image_tool::updater::{Result, UpdateError, platform, storage};
    use sha2::{Digest, Sha256};
    use std::{
        ffi::OsStr,
        fs::{File, OpenOptions},
        io::Write,
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        path::{Path, PathBuf},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
    };
    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }
    fn capture(path: &Path) -> serde_json::Value {
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        capture_file(&file)
    }
    fn capture_file(file: &File) -> serde_json::Value {
        let mut descriptor = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0);
        let mut text = ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut text,
                    ptr::null_mut(),
                )
            },
            0
        );
        let mut length = 0;
        unsafe {
            while *text.add(length) != 0 {
                length += 1;
            }
        }
        let sddl = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
            0
        );
        unsafe {
            LocalFree(text.cast());
            LocalFree(descriptor);
        }
        serde_json::json!({"sddl":sddl,"control":control,"protected":control & SE_DACL_PROTECTED != 0,"volume":info.dwVolumeSerialNumber,"index":(u64::from(info.nFileIndexHigh)<<32)|u64::from(info.nFileIndexLow),"links":info.nNumberOfLinks,"attrs":info.dwFileAttributes,"bytes":(u64::from(info.nFileSizeHigh)<<32)|u64::from(info.nFileSizeLow)})
    }
    fn unknown_writer_directory(path: &Path, owner: &str) {
        let text = format!(
            "O:{owner}D:P(A;OICI;FA;;;{owner})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1201bf;;;S-1-5-21-2648407538-2189114542-2988853691-2453461549)"
        );
        new_directory_from_sddl(path, &text);
    }
    fn new_directory_from_sddl(path: &Path, text: &str) {
        assert!(!path.exists());
        let mut descriptor = ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(text).as_ptr(),
                    1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            },
            0
        );
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let ok = unsafe { CreateDirectoryW(wide(path).as_ptr(), &attributes) };
        unsafe {
            LocalFree(descriptor);
        }
        assert_ne!(
            ok,
            0,
            "new fixture directory creation: {}",
            std::io::Error::last_os_error()
        );
    }
    fn record(
        rows: &mut Vec<serde_json::Value>,
        name: &str,
        passed: bool,
        evidence: serde_json::Value,
    ) {
        rows.push(serde_json::json!({"case":name,"passed":passed,"evidence":evidence}));
        assert!(passed, "native fixture case {name}");
    }
    pub fn run() -> Result<()> {
        let root = PathBuf::from(
            std::env::args_os()
                .nth(1)
                .expect("explicit fresh fixture root required"),
        );
        assert_eq!(root.parent(), Some(Path::new(r"C:\Windows\Temp")));
        let name = root.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("gloamere-preflight-native-")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
        assert!(!root.exists());
        let parents: Vec<_> = root
            .ancestors()
            .skip(1)
            .map(|p| (p.to_owned(), capture(p)))
            .collect();
        let mut rows = Vec::new();
        for access in [FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY] {
            let attempt = OpenOptions::new()
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&root);
            let error = attempt.as_ref().err().and_then(std::io::Error::raw_os_error);
            record(&mut rows, &format!("actual_directory_pin_blocks_new_writer_{access:x}"),
                error == Some(32), serde_json::json!({"access":access,"winerror":error}));
        }
        // Create another object, then open its writer before the product pin.
        // Only this new private object is involved; the root pins stay held.
        let preopened = root.join("preopened-data-writer");
        let owner = capture(&root)["sddl"].as_str().unwrap().split("D:").next().unwrap().to_owned();
        new_directory_from_sddl(&preopened, &format!("{owner}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})", current_user_sid()));
        let writer = OpenOptions::new().access_mode(FILE_ADD_FILE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&preopened)?;
        let before = capture(&preopened);
        let refused = private_paths::pin_existing_private_tree(&preopened, &preopened);
        record(&mut rows, "actual_candidate_rejects_preopened_data_writer",
            matches!(refused, Err(UpdateError::Io(ref e)) if e.raw_os_error() == Some(32)) && capture(&preopened)==before,
            serde_json::json!({"before":before,"unchanged":capture(&preopened)==before}));
        drop(writer);
        let reopened = private_paths::pin_existing_private_tree(&preopened, &preopened)?;
        record(&mut rows, "candidate_readmission_after_writer_close",
            capture(&preopened)==before, capture(&preopened));
        drop(reopened);
        let root_pins = private_paths::ensure_private_tree(&root, &root)?;
        let root_before = capture(&root);
        record(
            &mut rows,
            "new_root_admitted_through_actual_ancestors",
            root_before["protected"] == true,
            root_before.clone(),
        );
        let state = root.join("LightImageTool/updater-state");
        let state_pins = private_paths::ensure_private_tree(&root, &state)?;
        let before = capture(&state);
        let reuse = private_paths::ensure_private_tree(&root, &state)?;
        record(
            &mut rows,
            "reuse_has_exact_security_identity",
            capture(&state) == before,
            before.clone(),
        );
        let direct = private_paths::pin_existing_private_directory(&state)?;
        let all = private_paths::pin_existing_private_tree(&root, &state)?;
        let transaction = state.join("transaction");
        let transaction_pins = private_paths::create_private_directory_new(&transaction)?;
        let transaction_before = capture(&transaction);
        let collision = private_paths::create_private_directory_new(&transaction);
        record(
            &mut rows,
            "exclusive_collision_preserves_identity",
            matches!(collision,Err(UpdateError::Io(ref e)) if e.kind()==std::io::ErrorKind::AlreadyExists)
                && capture(&transaction) == transaction_before,
            transaction_before,
        );
        storage::atomic_json(
            &transaction.join("journal.json"),
            &serde_json::json!({"phase":"fixture-preparing","real_transaction":false}),
        )?;
        storage::atomic_json(
            &state.join("installer-active.json"),
            &serde_json::json!({"synthetic":true}),
        )?;
        for role in ["recovery-helper.exe", "backup-copy.bin", "restore-temp.bin"] {
            let mut temporary = platform::private_tempfile(&transaction)?;
            let created = capture_file(temporary.as_file());
            record(
                &mut rows,
                &format!("new_file_before_write:{role}"),
                created["protected"] == true && created["bytes"] == 0,
                created,
            );
            temporary.write_all(b"SYNTHETIC_NATIVE_FIXTURE_ONLY")?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(transaction.join(role))
                .map_err(|e| UpdateError::Io(e.error))?;
            record(
                &mut rows,
                &format!("published_file:{role}"),
                capture(&transaction.join(role))["protected"] == true,
                capture(&transaction.join(role)),
            );
        }
        let mut temp = private_paths::new_private_tempfile(&transaction)?;
        temp.write_all(b"DIRECT_NATIVE_TEMPFILE")?;
        temp.as_file().sync_all()?;
        let guard = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
        let lock = private_paths::open_private_lock(&state.join("sessions.lock"))?;
        let lock_before = capture_file(&lock);
        drop(lock);
        let lock = private_paths::open_private_lock(&state.join("sessions.lock"))?;
        record(
            &mut rows,
            "new_and_reopened_locks_private_unchanged",
            capture_file(&lock) == lock_before && lock_before["protected"] == true,
            lock_before.clone(),
        );
        drop(lock);
        let hard = state.join("hard.lock");
        std::fs::hard_link(state.join("sessions.lock"), &hard)?;
        let hard_before = capture(&hard);
        record(
            &mut rows,
            "hardlinked_lock_refused_unchanged",
            private_paths::open_private_lock(&hard).is_err() && capture(&hard) == hard_before,
            hard_before,
        );
        let nonempty = transaction.join("backup-copy.bin");
        let bytes = std::fs::read(&nonempty)?;
        record(
            &mut rows,
            "nonempty_lock_refused_without_truncation",
            private_paths::open_private_lock(&nonempty).is_err()
                && std::fs::read(&nonempty)? == bytes,
            serde_json::json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}),
        );
        let root_sddl = root_before["sddl"].as_str().unwrap();
        let owner = root_sddl
            .split('D')
            .next()
            .unwrap()
            .strip_prefix("O:")
            .unwrap();
        let foreign = root.join("unknown-writer");
        unknown_writer_directory(&foreign, owner);
        let foreign_before = capture(&foreign);
        record(
            &mut rows,
            "unknown_sid_writer_refused_42_unchanged",
            matches!(
                private_paths::pin_existing_private_tree(&root, &foreign),
                Err(UpdateError::PathUnsafeWriteAccess)
            ) && capture(&foreign) == foreign_before,
            foreign_before,
        );
        let group = root.join("group-owned");
        new_directory_from_sddl(&group, "O:BAD:P(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)");
        let group_before = capture(&group);
        record(
            &mut rows,
            "BA_owned_existing_object_refused_41_unchanged",
            matches!(
                private_paths::pin_existing_private_tree(&root, &group),
                Err(UpdateError::PathOwnerMismatch)
            ) && capture(&group) == group_before,
            group_before,
        );
        let rename_from = state.clone();
        let rename_to = root.join("replacement");
        let attempt = std::thread::spawn(move || std::fs::rename(rename_from, rename_to))
            .join()
            .unwrap();
        record(
            &mut rows,
            "cross_thread_directory_rename_blocked_by_real_pins",
            attempt
                .as_ref()
                .err()
                .is_some_and(|e| e.raw_os_error() == Some(32)),
            serde_json::json!({"winerror":attempt.err().and_then(|e|e.raw_os_error())}),
        );
        record(
            &mut rows,
            "all_existing_os_parent_security_identities_unchanged",
            parents.iter().all(|(p, before)| capture(p) == *before),
            serde_json::json!({"parents":parents}),
        );
        let files: Vec<_> = std::fs::read_dir(&transaction)?
            .map(|e| {
                let p = e.unwrap().path();
                serde_json::json!({"path":p,"identity":capture(&p)})
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"schema":1,"scope":"exact candidate native Win32 primitives; NOT full installer lifecycle","root":root,"passed":rows.len(),"failed":0,"real_installation_write":false,"registry_write":false,"existing_acl_setter":false,"rows":rows,"transaction_files":files})
        );
        drop((
            guard,
            temp,
            direct,
            all,
            reuse,
            transaction_pins,
            state_pins,
            root_pins,
        ));
        Ok(())
    }
}
#[cfg(windows)]
fn main() {
    fixture::run().unwrap();
}
#[cfg(not(windows))]
fn main() {
    panic!("This fixture requires actual Windows");
}
