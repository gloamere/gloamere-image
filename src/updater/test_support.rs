//! Filesystem fixtures with explicit Windows ownership, independent of TokenOwner.
//!
//! These helpers are compiled only for unit tests. They may initialize a fresh
//! fixture object, but must never repair a preexisting object's owner or DACL.
use std::{
    io,
    path::{Path, PathBuf},
};

pub fn tempdir() -> io::Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    initialize_new(directory.path())?;
    Ok(directory)
}

pub fn named_tempfile(parent: &Path) -> io::Result<tempfile::NamedTempFile> {
    #[cfg(windows)]
    return super::private_paths::fixture_tempfile(parent).map_err(io::Error::other);
    #[cfg(not(windows))]
    {
        let file = tempfile::NamedTempFile::new_in(parent)?;
        initialize_new(file.path())?;
        Ok(file)
    }
}

/// Initialize only an object the caller has just created for this fixture.
///
/// The caller must have obtained exclusive creation success, and must not pass
/// an existing object or a shared ancestor. Directories must still be empty so
/// the modern Windows setter cannot propagate changes to existing descendants.
pub fn initialize_new(path: impl AsRef<Path>) -> io::Result<()> {
    #[cfg(windows)]
    windows::initialize_new(path.as_ref())?;
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

/// Establish modern inherited-DACL semantics on a controlled fixture leaf.
///
/// This intentionally changes that leaf's descriptor for descriptor roundtrip
/// tests. The caller must own the fixture, and directories must still be empty.
/// It never rewrites the parent directory or another shared ancestor.
#[cfg(windows)]
pub fn normalize_inheritance(path: impl AsRef<Path>) -> io::Result<()> {
    windows::normalize_inheritance(path.as_ref())
}

pub mod fs {
    #[allow(unused_imports)]
    pub use std::fs::*;
    use std::{io, path::Path};

    pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        std::fs::create_dir(path)?;
        super::initialize_new(path)
    }

    pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path.is_dir() {
            return Ok(());
        }
        match create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or(error)?;
                create_dir_all(parent)?;
                match create_dir(path) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {
                        Ok(())
                    }
                    result => result,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn writable_file(path: &Path) -> io::Result<std::fs::File> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                super::initialize_new(path)?;
                Ok(file)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)
            }
            Err(error) => Err(error),
        }
    }

    pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        use io::Write;
        writable_file(path.as_ref())?.write_all(contents.as_ref())
    }

    pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
        let mut source = std::fs::File::open(from)?;
        let permissions = source.metadata()?.permissions();
        let to = to.as_ref();
        let mut destination = writable_file(to)?;
        // CopyFile can copy a Windows security descriptor. Fixture copying must
        // preserve an existing destination's owner/DACL even when it is unsafe.
        let copied = io::copy(&mut source, &mut destination)?;
        std::fs::set_permissions(to, permissions)?;
        Ok(copied)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::{OsStr, c_void},
        os::windows::{ffi::OsStrExt, fs::MetadataExt},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{Authorization::*, *},
        Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    struct Local(*mut c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value.as_ref().encode_wide().chain(Some(0)).collect()
    }
    fn checked_leaf(path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if path.file_name().is_none()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (metadata.is_dir() && std::fs::read_dir(path)?.next().is_some())
        {
            return Err(io::Error::other(
                "Expected an empty, non-reparse fixture leaf",
            ));
        }
        Ok(())
    }
    fn current_user_sid() -> io::Result<String> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _token = Handle(token);
        let mut length = 0;
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length) };
        if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 65536 {
            return Err(io::Error::other("Invalid fixture TokenUser size"));
        }
        let mut data = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                data.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
        let base = data.as_ptr() as usize;
        let end = base + data.len() * std::mem::size_of::<usize>();
        let sid = user.User.Sid as usize;
        if sid < base || sid.checked_add(8).is_none_or(|limit| limit > end) {
            return Err(io::Error::other("Invalid fixture TokenUser SID pointer"));
        }
        let sid_length = 8 + 4 * usize::from(unsafe { *user.User.Sid.cast::<u8>().add(1) });
        if sid.checked_add(sid_length).is_none_or(|limit| limit > end)
            || unsafe { IsValidSid(user.User.Sid) } == 0
        {
            return Err(io::Error::other("Invalid fixture TokenUser SID"));
        }
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _text = Local(text.cast());
        let mut count = 0;
        while count < 256 && unsafe { *text.add(count) } != 0 {
            count += 1;
        }
        if count == 256 {
            return Err(io::Error::other("Fixture SID string exceeds bound"));
        }
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, count)
        }))
    }
    fn set_descriptor(path: &Path, sddl: &str, owner: bool) -> io::Result<()> {
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
            return Err(io::Error::last_os_error());
        }
        let _descriptor = Local(descriptor);
        let mut sid = ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        let mut dacl = ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
            == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(io::Error::other("Missing fixture DACL"));
        }
        let mut information = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        if owner {
            if unsafe { GetSecurityDescriptorOwner(descriptor, &mut sid, &mut defaulted) } == 0
                || sid.is_null()
            {
                return Err(io::Error::other("Missing fixture owner"));
            }
            information |= OWNER_SECURITY_INFORMATION;
        }
        let error = unsafe {
            SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                information,
                sid,
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(())
    }
    pub(super) fn initialize_new(path: &Path) -> io::Result<()> {
        checked_leaf(path)?;
        let user = current_user_sid()?;
        let sddl = format!("O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
        // A standard token may own the object without initially having
        // WRITE_OWNER. Its implicit WRITE_DAC lets this fresh fixture grant
        // itself that right before the explicit owner assignment.
        set_descriptor(path, &sddl, false)?;
        set_descriptor(path, &sddl, true)
    }
    pub(super) fn normalize_inheritance(path: &Path) -> io::Result<()> {
        checked_leaf(path)?;
        let mut descriptor = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let error = unsafe {
            GetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let _descriptor = Local(descriptor);
        if dacl.is_null() {
            return Err(io::Error::other("Missing inherited fixture DACL"));
        }
        let error = unsafe {
            SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(())
    }

    #[test]
    fn existing_unsafe_descriptors_are_not_repaired_by_fixture_wrappers() {
        let directory = super::tempdir().unwrap();
        let child = directory.path().join("child");
        super::fs::create_dir(&child).unwrap();
        let file = child.join("existing.txt");
        super::fs::write(&file, b"old").unwrap();
        let source = directory.path().join("source.txt");
        super::fs::write(&source, b"replacement").unwrap();
        for path in [&child, &file] {
            set_descriptor(path, "D:P(A;OICI;FA;;;WD)", false).unwrap();
            assert!(crate::updater::platform::verify_current_user_path(path).is_err());
        }
        let before = [&child, &file].map(|path| {
            crate::updater::platform::capture_permissions(path)
                .unwrap()
                .windows_sddl
        });
        super::fs::create_dir_all(&child).unwrap();
        assert_eq!(
            super::fs::create_dir(&child).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        super::fs::write(&file, b"changed").unwrap();
        super::fs::copy(source, &file).unwrap();
        let after = [&child, &file].map(|path| {
            crate::updater::platform::capture_permissions(path)
                .unwrap()
                .windows_sddl
        });
        assert_eq!(before, after);
    }
}

#[test]
fn fresh_fixture_directories_and_files_pass_strict_owner_checks() {
    let directory = tempdir().unwrap();
    let child: PathBuf = directory.path().join("nested/child");
    fs::create_dir_all(&child).unwrap();
    let file = child.join("file.txt");
    fs::write(&file, b"fixture").unwrap();
    let copied = child.join("copy.txt");
    fs::copy(&file, &copied).unwrap();
    let temporary = named_tempfile(&child).unwrap();
    for path in [
        directory.path(),
        child.as_path(),
        &file,
        &copied,
        temporary.path(),
    ] {
        super::platform::verify_current_user_path(path).unwrap();
    }
}
