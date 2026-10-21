//! Bounded, durable, product-scoped state writes. No recursive cleanup APIs.
use super::{Result, UpdateError};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
#[cfg(any(not(windows), test))]
use std::fs::OpenOptions;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
pub fn plain_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir)) {
        return Err(UpdateError::Security(
            "必须使用没有上级跳转的绝对路径".into(),
        ));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(UpdateError::Security("拒绝重解析路径".into()));
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err(UpdateError::Security("拒绝符号链接".into()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn relative_path(value: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains(['\\', ':', '\0'])
        || value.starts_with('/')
    {
        return Err(UpdateError::Security("非法相对路径".into()));
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with([' ', '.'])
            || part.chars().any(|c| c.is_control())
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
        {
            return Err(UpdateError::Security("非法路径分量".into()));
        }
    }
    Ok(PathBuf::from(value))
}
pub fn owned_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = root.join(relative_path(relative)?);
    plain_path(&path)?;
    Ok(path)
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    plain_path(path)?;
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_STATE_BYTES {
        return Err(UpdateError::Security("状态文件超出范围".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(UpdateError::Security("状态文件过大".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn atomic_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(data)?;
    atomic_bytes(path, &bytes)
}
pub fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    plain_path(path)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(UpdateError::Security("状态数据过大".into()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Security("无父目录".into()))?;
    let _parent_pins = super::platform::ensure_private_data_directory(parent)?;
    let mut temp = super::platform::private_tempfile(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    plain_path(path)?;
    persist_replace(temp, path)?;
    sync_directory(parent)?;
    Ok(())
}
pub fn persist_replace(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    persist(temp, path, true)
}
pub fn persist_new(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    persist(temp, path, false)
}
fn persist(temp: tempfile::NamedTempFile, path: &Path, replace: bool) -> Result<()> {
    #[cfg(windows)]
    {
        super::private_publish::rename_same_directory(temp.as_file(), temp.path(), path, replace)?;
        drop(temp);
    }
    #[cfg(not(windows))]
    {
        if replace {
            temp.persist(path).map_err(|e| e.error)?;
        } else {
            temp.persist_noclobber(path).map_err(|e| e.error)?;
        }
    }
    Ok(())
}
pub fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path; // Windows publication flushes its write-through file handle.
    Ok(())
}
pub fn hash_file(path: &Path) -> Result<(u64, String)> {
    plain_path(path)?;
    let mut file = File::open(path)?;
    hash_reader(&mut file)
}
pub fn hash_reader(file: &mut File) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| UpdateError::Security("文件大小溢出".into()))?;
        hasher.update(&buffer[..read]);
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}
fn lock_error(error: std::fs::TryLockError) -> UpdateError {
    match error {
        std::fs::TryLockError::WouldBlock => {
            UpdateError::Deferred("其他实例或安装器持有更新锁".into())
        }
        std::fs::TryLockError::Error(error) => error.into(),
    }
}
pub struct LockGuard {
    _file: File,
    #[cfg(all(windows, not(test)))]
    _directories: super::private_paths::DirectoryPins,
}
impl LockGuard {
    pub fn wait_for(path: &Path, shared: bool, timeout: std::time::Duration) -> Result<Self> {
        let started = std::time::Instant::now();
        loop {
            match Self::acquire(path, shared) {
                Ok(guard) => return Ok(guard),
                Err(UpdateError::Deferred(_)) if started.elapsed() < timeout => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn acquire(path: &Path, shared: bool) -> Result<Self> {
        plain_path(path)?;
        #[cfg(all(windows, not(test)))]
        let directories = super::private_paths::pin_existing_private_directory(
            path.parent()
                .ok_or_else(|| UpdateError::Security("更新锁无父目录".into()))?,
        )?;
        #[cfg(all(windows, not(test)))]
        let file = super::private_paths::open_private_lock(path)?;
        #[cfg(any(not(windows), test))]
        let file = {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(windows)]
            {
                use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
                options.share_mode(3).custom_flags(0x00200000);
                let file = options.open(path)?;
                if !file.metadata()?.is_file() || file.metadata()?.file_attributes() & 0x400 != 0 {
                    return Err(UpdateError::Security("更新锁不是普通文件".into()));
                }
                file
            }
            #[cfg(not(windows))]
            options.open(path)?
        };
        if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        }
        .map_err(lock_error)?;
        Ok(Self {
            _file: file,
            #[cfg(all(windows, not(test)))]
            _directories: directories,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_real_lock_contention_is_deferred() {
        assert!(matches!(
            lock_error(std::fs::TryLockError::WouldBlock),
            UpdateError::Deferred(_)
        ));
        assert!(
            matches!(lock_error(std::fs::TryLockError::Error(std::io::ErrorKind::PermissionDenied.into())), UpdateError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
        );
    }
    #[test]
    fn rejects_windows_and_posix_escape_paths() {
        for bad in [
            "../x",
            "a/../x",
            "/x",
            "C:/x",
            "a\\x",
            "CON",
            "a/NUL.txt",
            "a./x",
            "a//x",
            "a/./x",
        ] {
            assert!(relative_path(bad).is_err(), "{bad}");
        }
        assert!(relative_path("runtime/gloamere-image-ui.exe").is_ok());
    }
    #[test]
    fn locks_compete_and_release() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("lock");
        let first = LockGuard::acquire(&path, true).unwrap();
        let second = LockGuard::acquire(&path, true).unwrap();
        assert!(LockGuard::acquire(&path, false).is_err());
        drop(first);
        drop(second);
        let exclusive = LockGuard::acquire(&path, false).unwrap();
        assert!(LockGuard::acquire(&path, true).is_err());
        drop(exclusive);
        assert!(LockGuard::acquire(&path, false).is_ok());
    }
    #[test]
    fn json_persistence_replaces_and_rejects_symlink() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        atomic_json(&p, &vec![1, 2]).unwrap();
        atomic_json(&p, &vec![3]).unwrap();
        assert_eq!(read_json::<Vec<u8>>(&p).unwrap(), vec![3]);
        #[cfg(unix)]
        {
            let link = d.path().join("link");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert!(atomic_json(&link, &vec![4]).is_err());
        }
    }
}
