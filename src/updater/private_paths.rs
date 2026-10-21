//! Candidate-only, bounded creation of private updater directory trees.
//!
//! The caller chooses the app boundary from the OS KnownFolder result, never
//! from untrusted CLI input, and retains `DirectoryPins` through path-based work.
//! Existing objects are only opened and audited: this module has no ACL setter,
//! ownership setter, token-default mutation, privilege adjustment or elevation.
//! Windows native standard/elevated-token acceptance remains unproven until the
//! isolated fixture matrix runs on Windows. Unix is a synthetic-test counterpart;
//! Unix directory handles do not provide Windows deny-delete-sharing semantics.
#![cfg_attr(all(not(windows), not(test)), allow(dead_code))]
use super::{Result, UpdateError};
use std::{
    fs::File,
    path::{Component, Path, PathBuf},
};

const MAX_COMPONENTS: usize = 128;

/// Keeps every audited namespace component alive. On Windows every handle also
/// denies write/delete sharing, preventing data-writer admission and ancestor
/// rename/replacement while retained. WRITE_ATTRIBUTES is not blocked by sharing:
/// foreign attribute rights remain rejected by the descriptor audit.
#[must_use = "retain directory pins for the entire path-based operation"]
#[derive(Debug)]
pub struct DirectoryPins {
    _directories: Vec<File>,
}

/// Ensure `leaf`, including missing components beginning at `app_root`, exists.
/// `app_root.parent()` must already exist. Existing app components require the
/// current TokenUser owner and the strict writer policy; they are never repaired.
/// A concurrent creator is treated as an existing object and fully audited.
/// On failure, already-created private directories remain; nothing is removed.
pub fn ensure_private_tree(app_root: &Path, leaf: &Path) -> Result<DirectoryPins> {
    walk(app_root, leaf, true)
}

/// The read-only form of `ensure_private_tree`: no missing directory is created.
pub fn pin_existing_private_tree(app_root: &Path, leaf: &Path) -> Result<DirectoryPins> {
    walk(app_root, leaf, false)
}

/// Exclusively create one new private directory under an existing private
/// parent. A name collision is an error, preserving transaction-ID exclusivity.
/// The returned pins include the parent namespace and the new directory.
#[cfg_attr(
    all(windows, test),
    allow(
        dead_code,
        reason = "Native installer unit fixtures replace exclusive directory creation; portable call-site tests are Unix-only"
    )
)]
pub fn create_private_directory_new(path: &Path) -> Result<DirectoryPins> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Security("Directory has no parent".into()))?;
    checked_components(parent, path)?;
    let mut pins = pin_existing_private_directory(parent)?;
    let identity = native::Identity::current()?;
    if !identity.create(path)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Private directory already exists",
        )
        .into());
    }
    pins._directories.push(identity.pin(path, true, true)?);
    Ok(pins)
}

/// Create a randomly named private file atomically with explicit ownership.
/// Initial parent pins are retained while creating; callers must additionally
/// retain their app-boundary pins through writing and durable publication.
/// Windows temp handles support same-directory publication by handle. A name
/// collision retries CREATE_NEW and never opens or modifies the existing file.
pub fn new_private_tempfile(parent: &Path) -> Result<tempfile::NamedTempFile> {
    let _pins = pin_existing_private_directory(parent)?;
    create_private_tempfile(parent)
}

// Transaction fixtures isolate their objects from the shared host namespace,
// while retaining exactly the production file creation and descriptor audit.
#[cfg(all(windows, test))]
pub(super) fn fixture_tempfile(parent: &Path) -> Result<tempfile::NamedTempFile> {
    create_private_tempfile(parent)
}

fn create_private_tempfile(parent: &Path) -> Result<tempfile::NamedTempFile> {
    let identity = native::Identity::current()?;
    let file = tempfile::Builder::new()
        .prefix(".gloamere-")
        .rand_bytes(16)
        .make_in(parent, |path| identity.create_new_file(path))?;
    identity.verify_new_file(file.as_file())?;
    Ok(file)
}

/// Opens an empty private lock without truncation or any content/ACL rewrite.
/// The caller must already retain audited pins for its parent directory.
/// Windows creation supplies explicit TokenUser ownership; existing files are
/// opened without following reparse points and audited through the same handle.
pub fn open_private_lock(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Security("Lock has no parent".into()))?;
    // Syntax only: the owning LockGuard retains the actual parent pins.
    checked_components(parent, path)?;
    native::Identity::current()?.open_lock(path)
}

/// Pin a generic existing private directory and audit its namespace ancestors.
/// Product-aware callers should instead pass their real first app component to
/// `pin_existing_private_tree`; this helper cannot infer an app boundary.
pub fn pin_existing_private_directory(root: &Path) -> Result<DirectoryPins> {
    pin_existing_private_tree(root, root)
}

fn checked_components(app_root: &Path, leaf: &Path) -> Result<Vec<PathBuf>> {
    for path in [app_root, leaf] {
        if !path.is_absolute()
            || path.components().count() > MAX_COMPONENTS
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            || path.as_os_str().len() > 32760
        {
            return Err(UpdateError::Security(
                "Invalid private directory boundary".into(),
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use std::path::Prefix;
            // This API handles local KnownFolder paths, never UNC, device or
            // verbatim aliases whose parsing can differ from normal Win32 paths.
            if !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)))
                || path.as_os_str().encode_wide().any(|c| c == 0)
            {
                return Err(UpdateError::Security(
                    "Private directory must use a local disk path".into(),
                ));
            }
            for part in path.components() {
                if let Component::Normal(name) = part {
                    let name = name
                        .to_str()
                        .ok_or_else(|| UpdateError::Security("Invalid directory name".into()))?;
                    super::storage::relative_path(name)?;
                }
            }
        }
    }
    if !leaf.starts_with(app_root)
        || app_root.parent().is_none_or(|p| p.as_os_str().is_empty())
        || app_root.file_name().is_none()
    {
        return Err(UpdateError::Security(
            "Private directory is outside its app boundary".into(),
        ));
    }
    let mut paths: Vec<_> = leaf
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect();
    paths.reverse();
    Ok(paths)
}

fn walk(app_root: &Path, leaf: &Path, create: bool) -> Result<DirectoryPins> {
    let paths = checked_components(app_root, leaf)?;
    let identity = native::Identity::current()?;
    let mut directories = Vec::with_capacity(paths.len());
    for path in paths {
        let private = path.starts_with(app_root);
        // Every existing root-to-leaf prefix is pinned and audited before the
        // first missing component can be created. A missing OS ancestor fails.
        let file = match identity.pin(&path, private, false) {
            Ok(file) => file,
            Err(UpdateError::Io(error))
                if create && private && error.kind() == std::io::ErrorKind::NotFound =>
            {
                create_and_pin(&identity, &path)?
            }
            Err(error) => return Err(error),
        };
        directories.push(file);
    }
    Ok(DirectoryPins {
        _directories: directories,
    })
}

fn create_and_pin(identity: &native::Identity, path: &Path) -> Result<File> {
    let created = identity.create(path)?;
    // SECURITY_ATTRIBUTES applies only at successful creation. AlreadyExists
    // is never authority to rewrite an existing owner or ACL.
    identity.pin(path, true, created)
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::{OsStr, c_void},
        os::windows::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE,
            INVALID_HANDLE_VALUE, LocalFree,
        },
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    // Same strict writer mask as the existing updater Windows owner verifier.
    const PRIVATE_WRITE_MASK: u32 = FILE_ADD_FILE
        | FILE_ADD_SUBDIRECTORY
        | FILE_WRITE_EA
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | GENERIC_ALL
        | GENERIC_WRITE;
    // Ancestors may permit creating children (ordinary C:\\ does), but 0x2
    // aliases FILE_WRITE_DATA and can set a reparse point on an empty directory.
    // Read-only lifetime sharing blocks both new/preopened data writers. Foreign
    // attribute, delete, ownership and DACL rights still require rejection.
    const ANCESTOR_WRITE_MASK: u32 = PRIVATE_WRITE_MASK & !(FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY);
    const TRUSTED_INSTALLER: &str =
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct Local(*mut c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    fn last_error() -> UpdateError {
        std::io::Error::last_os_error().into()
    }
    fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value.as_ref().encode_wide().chain(Some(0)).collect()
    }
    fn sid(value: &str) -> Result<Local> {
        let mut result = ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(wide(value).as_ptr(), &mut result) } == 0 {
            return Err(last_error());
        }
        Ok(Local(result))
    }

    pub(super) struct Identity {
        // usize allocation preserves TOKEN_USER and SID pointer alignment.
        user_data: Vec<usize>,
        system: Local,
        admins: Local,
        trusted_installer: Local,
        new_descriptor: Local,
    }
    impl Identity {
        pub(super) fn current() -> Result<Self> {
            let mut token = ptr::null_mut();
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(last_error());
            }
            let _token = Handle(token);
            let mut length = 0;
            unsafe {
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length);
            }
            if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 65536 {
                return Err(UpdateError::Security("Cannot query TokenUser".into()));
            }
            let mut user_data =
                vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    user_data.as_mut_ptr().cast(),
                    length,
                    &mut length,
                )
            } == 0
            {
                return Err(last_error());
            }
            let user = unsafe { &*user_data.as_ptr().cast::<TOKEN_USER>() };
            let base = user_data.as_ptr() as usize;
            let end = base + user_data.len() * std::mem::size_of::<usize>();
            let sid_base = user.User.Sid as usize;
            if length as usize > end - base
                || sid_base < base
                || sid_base.checked_add(8).is_none_or(|sid_end| sid_end > end)
            {
                return Err(UpdateError::Security(
                    "TokenUser SID is outside its buffer".into(),
                ));
            }
            let sid_len = 8 + 4 * usize::from(unsafe { *user.User.Sid.cast::<u8>().add(1) });
            if sid_base
                .checked_add(sid_len)
                .is_none_or(|sid_end| sid_end > end)
                || unsafe { IsValidSid(user.User.Sid) } == 0
            {
                return Err(UpdateError::Security("Invalid TokenUser SID".into()));
            }
            let mut text = ptr::null_mut();
            if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
                return Err(last_error());
            }
            let _text = Local(text.cast());
            let mut count = 0;
            unsafe {
                while count < 256 && *text.add(count) != 0 {
                    count += 1;
                }
            }
            if count == 256 {
                return Err(UpdateError::Security("TokenUser SID text too long".into()));
            }
            let user_sid =
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, count) });
            // Explicit TokenUser owner, protected DACL, inheritable only by this
            // trusted allowlist. TokenOwner/default DACL are never consulted.
            let sddl =
                format!("O:{user_sid}D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
            let mut descriptor = ptr::null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(sddl).as_ptr(),
                    1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_error());
            }
            let new_descriptor = Local(descriptor);
            Ok(Self {
                user_data,
                system: sid("S-1-5-18")?,
                admins: sid("S-1-5-32-544")?,
                trusted_installer: sid(TRUSTED_INSTALLER)?,
                new_descriptor,
            })
        }
        fn user(&self) -> PSID {
            unsafe { (*self.user_data.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
        fn trusted(&self, sid: PSID, private: bool) -> bool {
            unsafe {
                EqualSid(sid, self.user()) != 0
                    || EqualSid(sid, self.system.0) != 0
                    || EqualSid(sid, self.admins.0) != 0
                    || (!private && EqualSid(sid, self.trusted_installer.0) != 0)
            }
        }
        pub(super) fn create(&self, path: &Path) -> Result<bool> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.new_descriptor.0,
                bInheritHandle: 0,
            };
            if unsafe { CreateDirectoryW(wide(path).as_ptr(), &attributes) } != 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
                Ok(false)
            } else {
                Err(error.into())
            }
        }
        pub(super) fn pin(&self, path: &Path, private: bool, created: bool) -> Result<File> {
            let file = std::fs::OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(UpdateError::Security(
                    "Private namespace contains a reparse point or non-directory".into(),
                ));
            }
            self.audit(&file, private, created)?;
            Ok(file)
        }
        pub(super) fn create_new_file(&self, path: &Path) -> std::io::Result<File> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.new_descriptor.0,
                bInheritHandle: 0,
            };
            let handle = unsafe {
                CreateFileW(
                    wide(path).as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | READ_CONTROL | DELETE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    &attributes,
                    CREATE_NEW,
                    FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_handle(handle) })
        }
        pub(super) fn verify_new_file(&self, file: &File) -> Result<()> {
            let handle = file.as_raw_handle();
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { GetFileType(handle) } != FILE_TYPE_DISK
                || unsafe { GetFileInformationByHandle(handle, &mut info) } == 0
                || info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                    != 0
                || info.nNumberOfLinks != 1
                || info.nFileSizeHigh != 0
                || info.nFileSizeLow != 0
            {
                return Err(UpdateError::Security(
                    "New private file failed its handle audit".into(),
                ));
            }
            self.audit(file, true, true)
        }
        pub(super) fn open_lock(&self, path: &Path) -> Result<File> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.new_descriptor.0,
                bInheritHandle: 0,
            };
            // OPEN_ALWAYS never truncates. Explicit owner applies only if this
            // creates the object; Windows ignores SECURITY_ATTRIBUTES for an
            // existing object, which must pass the strict audit unchanged.
            let handle = unsafe {
                CreateFileW(
                    wide(path).as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    &attributes,
                    OPEN_ALWAYS,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error());
            }
            let created =
                std::io::Error::last_os_error().raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32);
            let file = unsafe { File::from_raw_handle(handle) };
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { GetFileType(handle) } != FILE_TYPE_DISK
                || unsafe { GetFileInformationByHandle(handle, &mut info) } == 0
            {
                return Err(UpdateError::Security(
                    "Lock is not a regular disk file".into(),
                ));
            }
            if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                != 0
                || info.nNumberOfLinks != 1
                || info.nFileSizeHigh != 0
                || info.nFileSizeLow != 0
            {
                return Err(UpdateError::Security(
                    "Lock must be empty, plain and singly linked".into(),
                ));
            }
            self.audit(&file, true, created)?;
            Ok(file)
        }
        fn audit(&self, file: &File, private: bool, created: bool) -> Result<()> {
            let mut owner = ptr::null_mut();
            let mut dacl = ptr::null_mut();
            let mut descriptor = ptr::null_mut();
            let status = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut owner,
                    ptr::null_mut(),
                    &mut dacl,
                    ptr::null_mut(),
                    &mut descriptor,
                )
            };
            if status != 0 {
                return Err(std::io::Error::from_raw_os_error(status as i32).into());
            }
            let _descriptor = Local(descriptor);
            self.audit_descriptor(descriptor, owner, dacl, private, created)
        }
        fn audit_descriptor(
            &self,
            descriptor: PSECURITY_DESCRIPTOR,
            owner: PSID,
            dacl: *mut ACL,
            private: bool,
            created: bool,
        ) -> Result<()> {
            if descriptor.is_null()
                || unsafe { IsValidSecurityDescriptor(descriptor) } == 0
                || unsafe { GetSecurityDescriptorLength(descriptor) } > 65536
            {
                return Err(UpdateError::PathUnsafeWriteAccess);
            }
            if owner.is_null()
                || unsafe { IsValidSid(owner) } == 0
                || if private {
                    (unsafe { EqualSid(owner, self.user()) }) == 0
                } else {
                    !self.trusted(owner, false)
                }
            {
                return Err(UpdateError::PathOwnerMismatch);
            }
            if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
                return Err(UpdateError::PathUnsafeWriteAccess);
            }
            if created {
                let mut control = 0;
                let mut revision = 0;
                if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
                    == 0
                {
                    return Err(last_error());
                }
                if control & SE_DACL_PROTECTED == 0 {
                    return Err(UpdateError::PathUnsafeWriteAccess);
                }
            }
            let mask = if private {
                PRIVATE_WRITE_MASK
            } else {
                ANCESTOR_WRITE_MASK
            };
            for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
                let mut ace = ptr::null_mut();
                if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                    return Err(last_error());
                }
                let header = unsafe { &*ace.cast::<ACE_HEADER>() };
                if !matches!(header.AceType, 0 | 1) || header.AceSize < 8 {
                    return Err(UpdateError::PathUnsafeWriteAccess);
                }
                // A deny entry cannot add rights. Ancestor-only inheritance
                // entries do not apply to that ancestor; explicit protected new
                // children never inherit them. App policy intentionally checks
                // inherit-only writers too, matching the existing strict guard.
                if header.AceType == 1
                    || (!private && u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0)
                {
                    continue;
                }
                let allowed = unsafe { ptr::read_unaligned(ace.cast::<u8>().add(4).cast::<u32>()) };
                if allowed & mask == 0 {
                    continue;
                }
                // Validate the embedded SID's full bounds before Win32 sees it.
                if header.AceSize < 16 {
                    return Err(UpdateError::PathUnsafeWriteAccess);
                }
                let sid_bytes = unsafe { ace.cast::<u8>().add(8) };
                let sid_len = 8 + 4 * usize::from(unsafe { *sid_bytes.add(1) });
                if sid_len > usize::from(header.AceSize) - 8 {
                    return Err(UpdateError::PathUnsafeWriteAccess);
                }
                let trustee = sid_bytes.cast();
                if unsafe { IsValidSid(trustee) } == 0 || !self.trusted(trustee, private) {
                    return Err(UpdateError::PathUnsafeWriteAccess);
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn user_text(identity: &Identity) -> String {
            let mut text = ptr::null_mut();
            assert_ne!(
                unsafe { ConvertSidToStringSidW(identity.user(), &mut text) },
                0
            );
            let _text = Local(text.cast());
            let mut len = 0;
            unsafe {
                while *text.add(len) != 0 {
                    len += 1;
                }
            }
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
        }
        fn audit_sddl(identity: &Identity, sddl: &str, private: bool) -> Result<()> {
            let mut descriptor = ptr::null_mut();
            assert_ne!(
                unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        wide(sddl).as_ptr(),
                        1,
                        &mut descriptor,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let _descriptor = Local(descriptor);
            let mut owner = ptr::null_mut();
            let mut dacl = ptr::null_mut();
            let mut defaulted = 0;
            let mut present = 0;
            assert_ne!(
                unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) },
                0
            );
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                },
                0
            );
            identity.audit_descriptor(descriptor, owner, dacl, private, false)
        }
        #[test]
        fn ancestor_create_only_is_allowed_but_private_write_is_rejected() {
            let identity = Identity::current().unwrap();
            let user = user_text(&identity);
            let sddl = format!("O:{user}D:P(A;;0x00000006;;;WD)(A;;FA;;;{user})");
            audit_sddl(&identity, &sddl, false).unwrap();
            assert!(matches!(
                audit_sddl(&identity, &sddl, true),
                Err(UpdateError::PathUnsafeWriteAccess)
            ));
        }
        #[test]
        fn foreign_destructive_control_and_reparse_rights_fail_for_ancestors() {
            let identity = Identity::current().unwrap();
            for mask in [
                FILE_WRITE_EA,
                FILE_DELETE_CHILD,
                FILE_WRITE_ATTRIBUTES,
                DELETE,
                WRITE_DAC,
                WRITE_OWNER,
                GENERIC_ALL,
                GENERIC_WRITE,
            ] {
                let sddl = format!("O:SYD:P(A;;0x{mask:08x};;;WD)(A;;FA;;;SY)");
                assert!(
                    matches!(
                        audit_sddl(&identity, &sddl, false),
                        Err(UpdateError::PathUnsafeWriteAccess)
                    ),
                    "mask={mask:08x}"
                );
            }
        }
        #[test]
        fn inherit_only_is_ignored_only_for_namespace_ancestors() {
            let identity = Identity::current().unwrap();
            let user = user_text(&identity);
            let sddl = format!("O:{user}D:P(A;OICIIO;FA;;;WD)(A;;FA;;;{user})");
            audit_sddl(&identity, &sddl, false).unwrap();
            assert!(matches!(
                audit_sddl(&identity, &sddl, true),
                Err(UpdateError::PathUnsafeWriteAccess)
            ));
        }
        #[test]
        fn administrators_owner_is_rejected_for_private_state() {
            let identity = Identity::current().unwrap();
            let sddl = "O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)";
            audit_sddl(&identity, sddl, false).unwrap();
            assert!(matches!(
                audit_sddl(&identity, sddl, true),
                Err(UpdateError::PathOwnerMismatch)
            ));
        }
        #[test]
        fn trusted_installer_is_only_a_namespace_trustee() {
            let identity = Identity::current().unwrap();
            let sddl = format!("O:{TRUSTED_INSTALLER}D:P(A;;FA;;;{TRUSTED_INSTALLER})");
            audit_sddl(&identity, &sddl, false).unwrap();
            assert!(matches!(
                audit_sddl(&identity, &sddl, true),
                Err(UpdateError::PathOwnerMismatch)
            ));
            let user = user_text(&identity);
            let sddl = format!("O:{user}D:P(A;;FA;;;{TRUSTED_INSTALLER})(A;;FA;;;{user})");
            assert!(matches!(
                audit_sddl(&identity, &sddl, true),
                Err(UpdateError::PathUnsafeWriteAccess)
            ));
        }
        #[test]
        fn created_private_objects_keep_explicit_ownership_and_directory_pins() {
            use std::io::Write;
            // Exercise explicit object creation independently of the host's
            // temporary-directory ancestors. Their policy is tested below.
            let temp = tempfile::tempdir().unwrap();
            let app = temp.path().join("LightImageTool");
            let state = app.join("updater-state");
            let identity = Identity::current().unwrap();
            assert!(identity.create(&app).unwrap());
            let app_pin = identity.pin(&app, true, true).unwrap();
            assert!(identity.create(&state).unwrap());
            let state_pin = identity.pin(&state, true, true).unwrap();
            let lock_path = state.join("state.lock");
            let lock = open_private_lock(&lock_path).unwrap();
            identity.audit(&lock, true, true).unwrap();
            let reopened_lock = identity.open_lock(&lock_path).unwrap();
            identity.audit(&reopened_lock, true, false).unwrap();
            let mut journal = tempfile::Builder::new()
                .prefix(".gloamere-")
                .rand_bytes(16)
                .make_in(&state, |path| identity.create_new_file(path))
                .unwrap();
            identity.verify_new_file(journal.as_file()).unwrap();
            journal.write_all(b"synthetic journal").unwrap();
            crate::updater::storage::persist_new(journal, &state.join("journal.json")).unwrap();
            let published = File::open(state.join("journal.json")).unwrap();
            identity.audit(&published, true, true).unwrap();
            drop(published);
            drop(reopened_lock);
            drop(lock);
            let renamed = temp.path().join("renamed");
            assert!(std::fs::rename(&app, &renamed).is_err());
            drop(state_pin);
            drop(app_pin);
            std::fs::rename(&app, &renamed).unwrap();
            assert_eq!(
                std::fs::read(renamed.join("updater-state/journal.json")).unwrap(),
                b"synthetic journal"
            );
        }
        #[test]
        fn full_namespace_creation_requires_every_existing_ancestor_to_pass() {
            let temp = tempfile::tempdir().unwrap();
            let app = temp.path().join("LightImageTool");
            let state = app.join("updater-state");
            let identity = Identity::current().unwrap();
            let mut ancestors: Vec<_> = temp
                .path()
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .collect();
            ancestors.reverse();
            let mut ancestor_pins = Vec::new();
            let mut expected_error = None;
            for path in ancestors {
                match identity.pin(path, false, false) {
                    Ok(pin) => ancestor_pins.push(pin),
                    Err(error) => {
                        expected_error = Some(error);
                        break;
                    }
                }
            }
            match expected_error {
                Some(expected) => {
                    let actual = ensure_private_tree(&app, &state).unwrap_err();
                    assert_eq!(
                        std::mem::discriminant(&actual),
                        std::mem::discriminant(&expected),
                        "full namespace rejected for a different reason: {actual}; expected {expected}"
                    );
                    if let (UpdateError::Io(actual), UpdateError::Io(expected)) = (actual, expected)
                    {
                        assert_eq!(actual.kind(), expected.kind());
                        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
                    }
                    assert!(!app.exists());
                    assert!(!state.exists());
                }
                None => {
                    let _pins = ensure_private_tree(&app, &state).unwrap();
                    let _app_pin = identity.pin(&app, true, true).unwrap();
                    let _state_pin = identity.pin(&state, true, true).unwrap();
                    let _existing = pin_existing_private_tree(&app, &state).unwrap();
                    let file = new_private_tempfile(&state).unwrap();
                    identity.verify_new_file(file.as_file()).unwrap();
                }
            }
        }
    }
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // POSIX identity rather than executable ownership (which can differ).
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    pub(super) struct Identity {
        uid: u32,
    }
    impl Identity {
        pub(super) fn current() -> Result<Self> {
            Ok(Self {
                uid: unsafe { geteuid() },
            })
        }
        pub(super) fn create(&self, path: &Path) -> Result<bool> {
            match std::fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
        pub(super) fn create_new_file(&self, path: &Path) -> std::io::Result<File> {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        }
        pub(super) fn verify_new_file(&self, file: &File) -> Result<()> {
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != 0 {
                return Err(UpdateError::Security(
                    "New private file failed its handle audit".into(),
                ));
            }
            if metadata.uid() != self.uid {
                return Err(UpdateError::PathOwnerMismatch);
            }
            if metadata.mode() & 0o022 != 0 {
                return Err(UpdateError::PathUnsafeWriteAccess);
            }
            Ok(())
        }
        pub(super) fn open_lock(&self, path: &Path) -> Result<File> {
            use std::os::unix::fs::OpenOptionsExt;
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(path)?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        return Err(UpdateError::Security("Lock is not a plain file".into()));
                    }
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)?
                }
                Err(error) => return Err(error.into()),
            };
            let metadata = file.metadata()?;
            let named = std::fs::symlink_metadata(path)?;
            if !metadata.is_file()
                || named.file_type().is_symlink()
                || metadata.nlink() != 1
                || metadata.len() != 0
                || (metadata.dev(), metadata.ino()) != (named.dev(), named.ino())
            {
                return Err(UpdateError::Security(
                    "Lock must be empty, plain and singly linked".into(),
                ));
            }
            if metadata.uid() != self.uid {
                return Err(UpdateError::PathOwnerMismatch);
            }
            if metadata.mode() & 0o022 != 0 {
                return Err(UpdateError::PathUnsafeWriteAccess);
            }
            Ok(file)
        }
        pub(super) fn pin(&self, path: &Path, private: bool, _created: bool) -> Result<File> {
            let before = std::fs::symlink_metadata(path)?;
            if before.file_type().is_symlink() || !before.is_dir() {
                return Err(UpdateError::Security(
                    "Private namespace contains a symlink or non-directory".into(),
                ));
            }
            let file = File::open(path)?;
            let metadata = file.metadata()?;
            let after = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir()
                || after.file_type().is_symlink()
                || (before.dev(), before.ino()) != (metadata.dev(), metadata.ino())
                || (after.dev(), after.ino()) != (metadata.dev(), metadata.ino())
            {
                return Err(UpdateError::Security(
                    "Private directory changed during audit".into(),
                ));
            }
            if (private && metadata.uid() != self.uid)
                || (!private && metadata.uid() != self.uid && metadata.uid() != 0)
            {
                return Err(UpdateError::PathOwnerMismatch);
            }
            let writable = metadata.mode() & 0o022 != 0;
            // Root-owned sticky /tmp is acceptable for synthetic private child
            // fixtures; sticky does not relax the app-owned directory policy.
            let sticky_namespace = !private && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            if writable && !sticky_namespace {
                return Err(UpdateError::PathUnsafeWriteAccess);
            }
            Ok(file)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod native {
    use super::*;
    pub(super) struct Identity;
    impl Identity {
        pub(super) fn current() -> Result<Self> {
            Err(UpdateError::Security(
                "Unsupported private-directory platform".into(),
            ))
        }
        pub(super) fn create(&self, _path: &Path) -> Result<bool> {
            unreachable!()
        }
        pub(super) fn create_new_file(&self, _path: &Path) -> std::io::Result<File> {
            unreachable!()
        }
        pub(super) fn verify_new_file(&self, _file: &File) -> Result<()> {
            unreachable!()
        }
        pub(super) fn open_lock(&self, _path: &Path) -> Result<File> {
            unreachable!()
        }
        pub(super) fn pin(&self, _path: &Path, _private: bool, _created: bool) -> Result<File> {
            unreachable!()
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("LightImageTool");
        let leaf = app.join("updater-state/snapshots/test");
        (directory, app, leaf)
    }
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }
    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn creates_private_components_and_retains_each_pin() {
        let (_directory, app, leaf) = fixture();
        let guard = ensure_private_tree(&app, &leaf).unwrap();
        assert!(guard._directories.len() >= 4);
        for path in leaf.ancestors().take(4) {
            assert_eq!(mode(path), 0o700);
        }
        let _existing = pin_existing_private_tree(&app, &leaf).unwrap();
        let _generic = pin_existing_private_directory(&leaf).unwrap();
    }
    #[test]
    fn existing_safe_objects_are_unchanged() {
        let (_directory, app, leaf) = fixture();
        let guard = ensure_private_tree(&app, &leaf).unwrap();
        let unrelated = app.join("unrelated.txt");
        std::fs::write(&unrelated, b"keep this").unwrap();
        chmod(&app, 0o750);
        let before = std::fs::metadata(&app).unwrap();
        let _second = ensure_private_tree(&app, &leaf).unwrap();
        let after = std::fs::metadata(&app).unwrap();
        assert_eq!(
            (before.dev(), before.ino(), before.mode()),
            (after.dev(), after.ino(), after.mode())
        );
        assert_eq!(std::fs::read(unrelated).unwrap(), b"keep this");
        drop(guard);
    }
    #[test]
    fn unsafe_app_parent_fails_before_creating_descendants() {
        let (_directory, app, leaf) = fixture();
        std::fs::create_dir(&app).unwrap();
        chmod(&app, 0o777);
        assert!(matches!(
            ensure_private_tree(&app, &leaf),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert!(!app.join("updater-state").exists());
        assert_eq!(mode(&app), 0o777);
    }
    #[test]
    fn unsafe_existing_leaf_is_never_repaired() {
        let (_directory, app, leaf) = fixture();
        let _guard = ensure_private_tree(&app, &leaf).unwrap();
        chmod(&leaf, 0o772);
        assert!(matches!(
            ensure_private_tree(&app, &leaf),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert_eq!(mode(&leaf), 0o772);
    }
    #[test]
    fn unsafe_namespace_parent_is_rejected_before_creation() {
        let (directory, app, leaf) = fixture();
        chmod(directory.path(), 0o777);
        assert!(matches!(
            ensure_private_tree(&app, &leaf),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert!(!app.exists());
        assert_eq!(mode(directory.path()), 0o777);
    }
    #[test]
    fn symlink_at_app_boundary_is_rejected_unchanged() {
        let (directory, app, leaf) = fixture();
        let outside = directory.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, &app).unwrap();
        assert!(ensure_private_tree(&app, &leaf).is_err());
        assert!(
            std::fs::symlink_metadata(&app)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!outside.join("updater-state").exists());
    }
    #[test]
    fn race_existing_unsafe_directory_is_audited_without_rewrite() {
        let (directory, _app, _leaf) = fixture();
        let race = directory.path().join("racing-child");
        std::fs::create_dir(&race).unwrap();
        chmod(&race, 0o777);
        let identity = native::Identity::current().unwrap();
        assert!(matches!(
            create_and_pin(&identity, &race),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert_eq!(mode(&race), 0o777);
    }
    #[test]
    fn race_existing_safe_directory_is_accepted_without_rewrite() {
        let (directory, _app, _leaf) = fixture();
        let race = directory.path().join("racing-child");
        std::fs::create_dir(&race).unwrap();
        chmod(&race, 0o750);
        let identity = native::Identity::current().unwrap();
        let _pin = create_and_pin(&identity, &race).unwrap();
        assert_eq!(mode(&race), 0o750);
    }
    #[test]
    fn race_existing_symlink_and_file_are_rejected() {
        let (directory, _app, _leaf) = fixture();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, b"unchanged").unwrap();
        let identity = native::Identity::current().unwrap();
        assert!(create_and_pin(&identity, &link).is_err());
        assert!(create_and_pin(&identity, &file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"unchanged");
    }
    #[test]
    fn missing_namespace_parent_is_not_created() {
        let (directory, _app, _leaf) = fixture();
        let missing = directory.path().join("missing");
        let app = missing.join("LightImageTool");
        assert!(ensure_private_tree(&app, &app.join("updater-state")).is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn read_only_variant_never_creates() {
        let (_directory, app, leaf) = fixture();
        assert!(pin_existing_private_tree(&app, &leaf).is_err());
        assert!(!app.exists());
    }
    #[test]
    fn exclusive_directory_collision_preserves_existing_object() {
        let (_directory, app, leaf) = fixture();
        let _pins = ensure_private_tree(&app, &leaf).unwrap();
        let child = leaf.join("unique-transaction");
        let _new = create_private_directory_new(&child).unwrap();
        assert_eq!(mode(&child), 0o700);
        let before = std::fs::metadata(&child).unwrap();
        assert!(
            matches!(create_private_directory_new(&child), Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(before.ino(), std::fs::metadata(&child).unwrap().ino());
    }
    #[test]
    fn private_tempfile_is_new_private_and_publishable() {
        use std::io::Write;
        let (_directory, app, leaf) = fixture();
        let _pins = ensure_private_tree(&app, &leaf).unwrap();
        let mut file = new_private_tempfile(&leaf).unwrap();
        assert_eq!(mode(file.path()), 0o600);
        file.write_all(b"private state").unwrap();
        let destination = leaf.join("journal.json");
        file.persist(&destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"private state");
    }
    #[test]
    fn private_tempfile_rejects_unsafe_parent_without_writes() {
        let (_directory, app, leaf) = fixture();
        let _pins = ensure_private_tree(&app, &leaf).unwrap();
        chmod(&leaf, 0o777);
        assert!(matches!(
            new_private_tempfile(&leaf),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert_eq!(std::fs::read_dir(&leaf).unwrap().count(), 0);
        assert_eq!(mode(&leaf), 0o777);
    }
    #[test]
    fn private_lock_creation_reopen_and_content_refusal() {
        let (_directory, app, leaf) = fixture();
        let _pins = ensure_private_tree(&app, &leaf).unwrap();
        let lock = leaf.join("update.lock");
        let first = open_private_lock(&lock).unwrap();
        assert_eq!(mode(&lock), 0o600);
        let _second = open_private_lock(&lock).unwrap();
        drop(first);
        std::fs::write(&lock, b"must stay unchanged").unwrap();
        assert!(open_private_lock(&lock).is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), b"must stay unchanged");
    }
    #[test]
    fn private_lock_rejects_symlink_hardlink_and_unsafe_mode() {
        let (_directory, app, leaf) = fixture();
        let _pins = ensure_private_tree(&app, &leaf).unwrap();
        let lock = leaf.join("update.lock");
        drop(open_private_lock(&lock).unwrap());
        let link = leaf.join("link.lock");
        symlink(&lock, &link).unwrap();
        assert!(open_private_lock(&link).is_err());
        chmod(&lock, 0o666);
        assert!(matches!(
            open_private_lock(&lock),
            Err(UpdateError::PathUnsafeWriteAccess)
        ));
        assert_eq!(mode(&lock), 0o666);
        chmod(&lock, 0o600);
        std::fs::hard_link(&lock, leaf.join("hard.lock")).unwrap();
        assert!(open_private_lock(&lock).is_err());
    }
    #[test]
    fn invalid_boundaries_and_depth_fail_before_creation() {
        let (directory, app, leaf) = fixture();
        assert!(ensure_private_tree(Path::new("relative"), &leaf).is_err());
        assert!(ensure_private_tree(&app, directory.path()).is_err());
        assert!(ensure_private_tree(&app, &app.join("../outside")).is_err());
        let mut deep = app.clone();
        for _ in 0..MAX_COMPONENTS {
            deep.push("nested");
        }
        assert!(ensure_private_tree(&app, &deep).is_err());
        assert!(!app.exists());
    }
}
