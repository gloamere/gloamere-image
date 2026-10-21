//! Same-directory publication without reopening a write-denied parent directory.
use super::{Result, UpdateError, storage};
use std::{
    ffi::OsString,
    fs::File,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::{
    Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    },
    Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TEMPORARY, FILE_BASIC_INFO,
            FILE_FLAG_WRITE_THROUGH, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FileBasicInfo,
            GetFileInformationByHandle, GetFileInformationByHandleEx, GetFileType,
            GetFinalPathNameByHandleW, READ_CONTROL, ReOpenFile, SetFileInformationByHandle,
            VOLUME_NAME_DOS,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const MAX_UTF16_PATH: usize = 32760;

fn security(message: &str) -> UpdateError {
    UpdateError::Security(message.into())
}

fn final_path(file: &File) -> Result<PathBuf> {
    let mut buffer = vec![0u16; MAX_UTF16_PATH + 1];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if length == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let length = length as usize;
    if length > MAX_UTF16_PATH || length >= buffer.len() {
        return Err(security("Publication source path exceeds its bound"));
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

fn check_regular_file(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(security("Publication source is not a disk file"));
    }
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || information.nNumberOfLinks != 1
    {
        return Err(security(
            "Publication source must be plain and singly linked",
        ));
    }
    Ok(information)
}

/// Atomically rename an already-open file to a leaf in its existing directory.
/// Callers retain their audited parent pins throughout the operation.
pub(super) fn rename_same_directory(
    file: &File,
    source: &Path,
    destination: &Path,
    replace: bool,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    storage::plain_path(source)?;
    storage::plain_path(destination)?;
    if source == destination || source.parent().is_none() || source.parent() != destination.parent()
    {
        return Err(security("Publication must stay in the source directory"));
    }
    for path in [source, destination] {
        let mut length = 0usize;
        for unit in path.as_os_str().encode_wide() {
            length += 1;
            if unit == 0 || length > MAX_UTF16_PATH {
                return Err(security("Invalid or oversized publication path"));
            }
        }
    }
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| security("Publication target has no valid leaf name"))?;
    let checked = storage::relative_path(leaf)?;
    if checked.components().count() != 1 {
        return Err(security("Publication target must be one leaf name"));
    }
    let source_information = check_regular_file(file)?;
    if final_path(file)? != std::fs::canonicalize(source)? {
        return Err(security(
            "Publication source path differs from its open handle",
        ));
    }
    if destination.exists() {
        use std::os::windows::fs::OpenOptionsExt;
        let target = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(destination)?;
        let target_information = check_regular_file(&target)?;
        if source_information.dwVolumeSerialNumber == target_information.dwVolumeSerialNumber
            && source_information.nFileIndexHigh == target_information.nFileIndexHigh
            && source_information.nFileIndexLow == target_information.nFileIndexLow
        {
            return Err(security("Publication cannot retain the source filename"));
        }
    }

    // ReOpenFile retains the identity of the actual source object. Opening by
    // source path again could select a replacement between verification and use.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            GENERIC_WRITE | DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_WRITE_THROUGH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let publication = unsafe { File::from_raw_handle(handle) };
    let information = check_regular_file(&publication)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_TEMPORARY != 0 {
        let mut basic = FILE_BASIC_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                publication.as_raw_handle(),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        basic.FileAttributes &= !FILE_ATTRIBUTE_TEMPORARY;
        if basic.FileAttributes == 0 {
            basic.FileAttributes = FILE_ATTRIBUTE_NORMAL;
        }
        if unsafe {
            SetFileInformationByHandle(
                publication.as_raw_handle(),
                FileBasicInfo,
                (&basic as *const FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    publication.sync_all()?;

    let name: Vec<u16> = leaf.encode_utf16().collect();
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| security("Publication target length overflows"))?;
    let name_offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    let buffer_bytes = name_offset
        .checked_add(name_bytes as usize)
        .map(|length| length.max(size_of::<FILE_RENAME_INFORMATION>()))
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| security("Publication rename buffer overflows"))?;
    // usize storage supplies the alignment required by the native HANDLE field.
    let mut buffer = vec![0usize; (buffer_bytes as usize).div_ceil(size_of::<usize>())];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = replace;
        (*information).RootDirectory = ptr::null_mut();
        (*information).FileNameLength = name_bytes;
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer
                .as_mut_ptr()
                .cast::<u8>()
                .add(name_offset)
                .cast::<u16>(),
            name.len(),
        );
    }
    let mut status = IO_STATUS_BLOCK::default();
    // An absolute target or non-null RootDirectory would cause another
    // FILE_ADD_FILE open of the parent, conflicting with its deny-write pin.
    // Native same-directory leaf rename uses the source's existing parent.
    let result = unsafe {
        NtSetInformationFile(
            publication.as_raw_handle(),
            &mut status,
            information.cast(),
            buffer_bytes,
            FileRenameInformation,
        )
    };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32
        )
        .into());
    }
    let completion = unsafe { status.Anonymous.Status };
    if completion != 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(completion) } as i32,
        )
        .into());
    }
    // The write-through source handle and final flush also cover the NTFS
    // metadata changed by publication; no parent pin is released to publish.
    publication.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::test_support::{self, fs};
    use std::{
        io::Write,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    };

    fn parent_pin(path: &Path) -> File {
        std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }

    #[test]
    fn publication_succeeds_while_parent_pin_rejects_write_and_delete_admission() {
        let directory = test_support::tempdir().unwrap();
        let parent = directory.path().join("state");
        fs::create_dir(&parent).unwrap();
        let pin = parent_pin(&parent);
        let target = parent.join("journal.json");
        for (replace, contents) in [(false, b"first".as_slice()), (true, b"second".as_slice())] {
            let mut temporary = if replace {
                test_support::named_tempfile(&parent).unwrap()
            } else {
                let temporary = tempfile::NamedTempFile::new_in(&parent).unwrap();
                assert_ne!(
                    temporary.as_file().metadata().unwrap().file_attributes()
                        & FILE_ATTRIBUTE_TEMPORARY,
                    0
                );
                temporary
            };
            temporary.write_all(contents).unwrap();
            rename_same_directory(temporary.as_file(), temporary.path(), &target, replace).unwrap();
            assert_eq!(fs::read(&target).unwrap(), contents);
            assert_eq!(
                fs::metadata(&target).unwrap().file_attributes() & FILE_ATTRIBUTE_TEMPORARY,
                0
            );
            assert!(fs::rename(&parent, directory.path().join("renamed")).is_err());
            assert_eq!(
                std::fs::OpenOptions::new()
                    .access_mode(FILE_ADD_FILE)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(&parent)
                    .unwrap_err()
                    .raw_os_error(),
                Some(32)
            );
        }
        drop(pin);
    }

    #[test]
    fn nonreplacement_collision_preserves_both_files() {
        let directory = test_support::tempdir().unwrap();
        let pin = parent_pin(directory.path());
        let target = directory.path().join("existing.json");
        fs::write(&target, b"original").unwrap();
        let mut temporary = test_support::named_tempfile(directory.path()).unwrap();
        temporary.write_all(b"new").unwrap();
        assert!(
            rename_same_directory(temporary.as_file(), temporary.path(), &target, false).is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read(temporary.path()).unwrap(), b"new");
        drop(pin);
    }

    #[test]
    fn cross_directory_parent_traversal_and_wrong_source_handle_are_rejected() {
        let directory = test_support::tempdir().unwrap();
        let other = directory.path().join("other");
        fs::create_dir(&other).unwrap();
        let mut temporary = test_support::named_tempfile(directory.path()).unwrap();
        temporary.write_all(b"untouched").unwrap();
        for destination in [
            other.join("target.json"),
            directory.path().join("other/../target.json"),
        ] {
            assert!(
                rename_same_directory(temporary.as_file(), temporary.path(), &destination, false,)
                    .is_err()
            );
            assert!(!destination.exists());
        }
        for destination in [
            temporary.path().to_path_buf(),
            temporary.path().with_file_name(
                temporary
                    .path()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_uppercase(),
            ),
        ] {
            assert!(
                rename_same_directory(temporary.as_file(), temporary.path(), &destination, true,)
                    .is_err()
            );
        }
        let different = directory.path().join("different");
        fs::write(&different, b"different source").unwrap();
        assert!(
            rename_same_directory(
                temporary.as_file(),
                &different,
                &directory.path().join("target.json"),
                false,
            )
            .is_err()
        );
        assert_eq!(fs::read(temporary.path()).unwrap(), b"untouched");
        assert_eq!(fs::read(different).unwrap(), b"different source");
    }
}
