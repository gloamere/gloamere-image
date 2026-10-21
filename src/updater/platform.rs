//! Windows adapters keep OS security, user ownership and fixed installer args.
use super::{Result, UpdateError, storage};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    path::{Path, PathBuf},
};
#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePermissions {
    pub readonly: bool,
    pub unix_mode: Option<u32>,
    pub windows_sddl: Option<String>,
}
pub fn capture_permissions(path: &Path) -> Result<FilePermissions> {
    storage::plain_path(path)?;
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode())
    };
    #[cfg(not(unix))]
    let unix_mode = None;
    #[cfg(windows)]
    let windows_sddl = Some(windows::file_sddl(path)?);
    #[cfg(not(windows))]
    let windows_sddl = None;
    Ok(FilePermissions {
        readonly: metadata.permissions().readonly(),
        unix_mode,
        windows_sddl,
    })
}
pub fn restore_permissions(path: &Path, permissions: &FilePermissions) -> Result<()> {
    storage::plain_path(path)?;
    #[cfg(unix)]
    if let Some(mode) = permissions.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    {
        let sddl = permissions
            .windows_sddl
            .as_ref()
            .ok_or_else(|| UpdateError::Repair("缺少原文件安全描述符".into()))?;
        windows::restore_sddl(path, sddl)?;
        let mut p = std::fs::metadata(path)?.permissions();
        if p.readonly() != permissions.readonly {
            p.set_readonly(permissions.readonly);
            std::fs::set_permissions(path, p)?;
        }
    }
    Ok(())
}
pub fn verify_current_user_path(path: &Path) -> Result<()> {
    storage::plain_path(path)?;
    #[cfg(windows)]
    windows::verify_owner(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let this = std::fs::metadata(std::env::current_exe()?)?.uid();
        let owner = std::fs::metadata(path)?.uid();
        if this != owner {
            return Err(UpdateError::Security("安装目录不属于当前用户".into()));
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryValue {
    pub key: String,
    pub name: String,
    pub kind: u32,
    pub bytes: Option<Vec<u8>>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationSnapshot {
    pub registry: Vec<RegistryValue>,
}
impl IntegrationSnapshot {
    pub fn capture(installation: &Path) -> Result<Self> {
        #[cfg(all(windows, not(test)))]
        {
            windows::capture_integration(installation)
        }
        #[cfg(any(not(windows), test))]
        {
            let _ = installation;
            Ok(Self::default())
        }
    }
    pub fn validate(&self, _installation: &Path) -> Result<()> {
        if self.registry.len() > 256 {
            return Err(UpdateError::Security("登记快照过大".into()));
        }
        for r in &self.registry {
            if !registry_pair_allowed(&r.key, &r.name)
                || !matches!(r.kind, 0 | 1 | 2 | 4)
                || r.bytes.as_ref().is_some_and(|b| b.len() > 65536)
            {
                return Err(UpdateError::Security("登记恢复路径或值无效".into()));
            }
        }
        Ok(())
    }
    pub fn restore(&self, installation: &Path) -> Result<()> {
        self.validate(installation)?;
        #[cfg(all(windows, not(test)))]
        windows::restore_integration(self, installation)?;
        #[cfg(test)]
        let _ = installation;
        Ok(())
    }
}
pub fn registry_pairs() -> Vec<(String, String)> {
    let uninstall = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}_is1",
        super::PRODUCT_ID
    );
    let mut pairs = Vec::new();
    for name in [
        "DisplayName",
        "DisplayVersion",
        "InstallLocation",
        "DisplayIcon",
        "UninstallString",
        "QuietUninstallString",
        "Publisher",
        "Inno Setup: App Path",
        "Inno Setup: Icon Group",
        "Inno Setup: User",
        "Inno Setup: Selected Tasks",
        "Inno Setup: Deselected Tasks",
        "Inno Setup: Setup Version",
        "Inno Setup: Language",
        "NoModify",
        "NoRepair",
        "EstimatedSize",
        "InstallDate",
    ] {
        pairs.push((uninstall.clone(), name.into()));
    }
    let root = "Software\\LightImageTool\\DefaultApps";
    let prog = "Software\\Classes\\LightImageTool.Image";
    for name in ["OwnerId", "OwnerPath"] {
        pairs.push((root.into(), name.into()));
        pairs.push((prog.into(), name.into()));
    }
    for name in [
        "ApplicationName",
        "ApplicationDescription",
        "ApplicationIcon",
    ] {
        pairs.push((format!("{root}\\Capabilities"), name.into()));
    }
    for ext in [".jpg", ".jpeg", ".png", ".gif", ".webp"] {
        pairs.push((
            format!("{root}\\Capabilities\\FileAssociations"),
            ext.into(),
        ));
        let menu =
            format!("Software\\Classes\\SystemFileAssociations\\{ext}\\shell\\LightImageTool");
        for name in [
            "OwnerId",
            "OwnerPath",
            "MUIVerb",
            "Icon",
            "MultiSelectModel",
        ] {
            pairs.push((menu.clone(), name.into()));
        }
        pairs.push((format!("{menu}\\command"), String::new()));
    }
    for suffix in ["", "\\DefaultIcon", "\\shell\\open\\command"] {
        pairs.push((format!("{prog}{suffix}"), String::new()));
    }
    for name in ["gloamere-image", "Light Image Tool"] {
        pairs.push(("Software\\RegisteredApplications".into(), name.into()));
    }
    pairs
}
fn registry_pair_allowed(key: &str, name: &str) -> bool {
    registry_pairs().iter().any(|(k, n)| k == key && n == name)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherPolicy {
    RequireCertificate { sha256: String },
    TufAuthenticatedUnsigned,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherVerification {
    Authenticode { certificate_sha256: String },
    UnsignedTufAuthenticated,
}
/// The returned File stays open without write/delete sharing on Windows until
/// the child Setup exits. Hash and publisher verification refer to that handle.
pub struct ExecutionLease {
    pub(crate) directories: Vec<File>,
    pub(crate) file: File,
    pub(crate) path: PathBuf,
    pub verification: PublisherVerification,
}
impl ExecutionLease {
    pub fn verify(
        path: &Path,
        config: &safeupdate_core::ProductionConfig,
        target: &safeupdate_core::VerifiedTarget,
        state: &mut safeupdate_core::LockedState,
    ) -> Result<Self> {
        safeupdate_core::revalidate_for_execution(config, target, state)
            .map_err(|e| UpdateError::Security(e.to_string()))?;
        let identity = super::recovery::FileIdentity {
            length: target.length(),
            sha256: target.sha256().iter().map(|v| format!("{v:02x}")).collect(),
        };
        let policy = match config.publisher_policy() {
            safeupdate_core::PublisherPolicy::ExactAuthenticodeCertificate(hash) => {
                PublisherPolicy::RequireCertificate {
                    sha256: hash.iter().map(|v| format!("{v:02x}")).collect(),
                }
            }
            safeupdate_core::PublisherPolicy::TufAuthenticatedUnsigned => {
                PublisherPolicy::TufAuthenticatedUnsigned
            }
        };
        storage::plain_path(path)?;
        verify_current_user_path(
            path.parent()
                .ok_or_else(|| UpdateError::Security("包路径无效".into()))?,
        )?;
        #[cfg(windows)]
        {
            windows::verify_package(path, &identity, &policy)
        }
        #[cfg(not(windows))]
        {
            let _ = (identity, policy);
            Err(UpdateError::Deferred(
                "EXE 发布者校验与执行仅在 Windows 支持".into(),
            ))
        }
    }
    pub(crate) fn install(
        self,
        config: &safeupdate_core::ProductionConfig,
        target: &safeupdate_core::VerifiedTarget,
        trust: &mut safeupdate_core::LockedState,
        installation: &Path,
        log: &Path,
    ) -> Result<i32> {
        storage::plain_path(installation)?;
        storage::plain_path(log)?;
        let _keep_handle = (&self.file, &self.directories);
        let mut file = self.file.try_clone()?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        let (length, hash) = storage::hash_reader(&mut file)?;
        let expected: String = target.sha256().iter().map(|v| format!("{v:02x}")).collect();
        if length != target.length() || hash != expected {
            return Err(UpdateError::Security("执行租约与可信目标不一致".into()));
        }
        // Trust-state lock stays held across this final check and CreateProcess.
        safeupdate_core::revalidate_for_execution(config, target, trust)
            .map_err(|e| UpdateError::Security(e.to_string()))?;
        #[cfg(windows)]
        {
            let status = std::process::Command::new(&self.path)
                .args(installer_arguments(installation, log))
                .status()?;
            Ok(status.code().unwrap_or(-1))
        }
        #[cfg(not(windows))]
        {
            let _ = &self.path;
            Err(UpdateError::Deferred(
                "此平台不能运行 Windows 安装器".into(),
            ))
        }
    }
}
pub fn installer_arguments(installation: &Path, log: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "/VERYSILENT",
        "/SUPPRESSMSGBOXES",
        "/SP-",
        "/NORESTART",
        "/NOCLOSEAPPLICATIONS",
        "/NOFORCECLOSEAPPLICATIONS",
        "/NORESTARTAPPLICATIONS",
        "/RESTARTEXITCODE=3010",
    ]
    .map(Into::into)
    .to_vec();
    let mut dir = std::ffi::OsString::from("/DIR=");
    dir.push(installation);
    args.push(dir);
    let mut logfile = std::ffi::OsString::from("/LOG=");
    logfile.push(log);
    args.push(logfile);
    args
}
pub fn reboot_operations_clear() -> Result<bool> {
    #[cfg(all(windows, not(test)))]
    {
        windows::reboot_operations_clear()
    }
    #[cfg(any(not(windows), test))]
    {
        Ok(true)
    } // Controlled filesystem fixture adapter only; no EXE execution on this platform.
}
pub fn network_is_metered_or_unknown() -> bool {
    #[cfg(windows)]
    {
        windows::metered_network()
    }
    #[cfg(not(windows))]
    {
        true
    }
}
pub fn download_resources(root: &Path, heavy_work: bool) -> Result<super::client::Resources> {
    #[cfg(windows)]
    {
        windows::resources(root, heavy_work)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, heavy_work);
        Err(UpdateError::Deferred("原生资源策略尚未支持此平台".into()))
    }
}
static SESSION_ENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn session_ending() -> bool {
    SESSION_ENDING.load(std::sync::atomic::Ordering::Acquire)
}
pub fn install_shutdown_observer(context: &eframe::CreationContext<'_>) -> Result<()> {
    #[cfg(windows)]
    {
        use raw_window_handle::HasWindowHandle;
        if let Ok(handle) = context.window_handle()
            && let raw_window_handle::RawWindowHandle::Win32(window) = handle.as_raw()
        {
            return windows::observe_shutdown(window.hwnd.get() as *mut _);
        }
        Err(UpdateError::Deferred(
            "无法确认系统关闭信号，本次不安排退出安装".into(),
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = context;
        Ok(())
    }
}

pub fn spawn_exit_helper(helper: &Path, installation: &Path, nonce: &str) -> Result<()> {
    #[cfg(windows)]
    {
        windows::spawn_exit_helper(helper, installation, nonce)
    }
    #[cfg(not(windows))]
    {
        let _ = (helper, installation, nonce);
        Err(UpdateError::Deferred(
            "退出后 EXE 更新仅支持 Windows".into(),
        ))
    }
}
pub fn wait_exit_parent(pid: u32, creation: u64, installation: &Path, nonce: &str) -> Result<()> {
    #[cfg(windows)]
    {
        windows::wait_exit_parent(pid, creation, installation, nonce)
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, creation, installation, nonce);
        Err(UpdateError::Deferred("退出握手仅支持 Windows".into()))
    }
}

/// Pins every existing Windows namespace ancestor against delete/rename while a
/// verified executable or install transaction uses path-based Win32 operations.
pub fn pin_directories(path: &Path) -> Result<Vec<File>> {
    storage::plain_path(path)?;
    #[cfg(windows)]
    {
        windows::pin_directories(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(Vec::new())
    }
}

pub fn current_user_update_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        windows::current_user_update_root()
    }
    #[cfg(not(windows))]
    {
        crate::settings::user_settings_path()
            .and_then(|p| p.parent().map(|p| p.join("updater-state")))
            .ok_or_else(|| UpdateError::Security("当前用户更新目录不可用".into()))
    }
}

/// A fixed current-user update namespace, held against rename while in use.
pub struct StateDirectoryGuard {
    #[cfg(all(windows, not(test)))]
    _pins: super::private_paths::DirectoryPins,
}
pub fn ensure_update_directory(path: &Path) -> Result<StateDirectoryGuard> {
    #[cfg(all(windows, not(test)))]
    {
        let state = windows::current_user_update_root()?;
        if !path.starts_with(&state) {
            return Err(UpdateError::Security(
                "更新目录不属于固定当前用户命名空间".into(),
            ));
        }
        let app_root = state
            .parent()
            .ok_or_else(|| UpdateError::Security("更新目录无父路径".into()))?;
        let pins = super::private_paths::ensure_private_tree(app_root, path)?;
        Ok(StateDirectoryGuard { _pins: pins })
    }
    #[cfg(any(not(windows), test))]
    {
        // Transaction fixtures use isolated roots, including on hosts whose
        // shared temp ancestors deliberately fail the native namespace audit.
        storage::plain_path(path)?;
        #[cfg(all(windows, test))]
        super::test_support::fs::create_dir_all(path)?;
        #[cfg(not(windows))]
        std::fs::create_dir_all(path)?;
        Ok(StateDirectoryGuard {})
    }
}

/// Create only missing internal data directories under an already private parent.
/// Existing ownership/ACLs are audited, never rewritten. Caller retains the guard.
pub fn ensure_private_data_directory(path: &Path) -> Result<StateDirectoryGuard> {
    #[cfg(all(windows, not(test)))]
    {
        storage::plain_path(path)?;
        let mut existing = path;
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| UpdateError::Security("私有目录无现存父路径".into()))?;
        }
        let pins = super::private_paths::ensure_private_tree(existing, path)?;
        Ok(StateDirectoryGuard { _pins: pins })
    }
    #[cfg(any(not(windows), test))]
    {
        storage::plain_path(path)?;
        #[cfg(all(windows, test))]
        super::test_support::fs::create_dir_all(path)?;
        #[cfg(not(windows))]
        std::fs::create_dir_all(path)?;
        Ok(StateDirectoryGuard {})
    }
}
pub fn create_private_data_directory_new(path: &Path) -> Result<StateDirectoryGuard> {
    #[cfg(all(windows, not(test)))]
    {
        Ok(StateDirectoryGuard {
            _pins: super::private_paths::create_private_directory_new(path)?,
        })
    }
    #[cfg(any(not(windows), test))]
    {
        storage::plain_path(path)?;
        #[cfg(all(windows, test))]
        super::test_support::fs::create_dir(path)?;
        #[cfg(not(windows))]
        std::fs::create_dir(path)?;
        Ok(StateDirectoryGuard {})
    }
}
pub fn private_tempfile(parent: &Path) -> Result<tempfile::NamedTempFile> {
    #[cfg(all(windows, not(test)))]
    {
        super::private_paths::new_private_tempfile(parent)
    }
    #[cfg(any(not(windows), test))]
    {
        #[cfg(all(windows, test))]
        return Ok(super::test_support::named_tempfile(parent)?);
        #[cfg(not(windows))]
        Ok(tempfile::NamedTempFile::new_in(parent)?)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::super::test_support;
    use super::*;

    #[cfg(windows)]
    #[test]
    fn inherited_windows_descriptor_roundtrip_is_exact() {
        let directory = test_support::tempdir().unwrap();
        let file = directory.path().join("owned-acl-fixture.txt");
        test_support::fs::write(&file, b"synthetic owned file").unwrap();
        test_support::normalize_inheritance(&file).unwrap();
        let original = capture_permissions(&file).unwrap();
        assert!(original.windows_sddl.as_ref().unwrap().contains("D:AI"));
        restore_permissions(&file, &original).unwrap();
        assert_eq!(
            capture_permissions(&file).unwrap().windows_sddl,
            original.windows_sddl
        );
    }
    #[test]
    fn arguments_are_local_fixed_no_forced_close_or_task_reset() {
        let args = installer_arguments(Path::new("C:/中文 app"), Path::new("C:/updates/log.txt"));
        let strings: Vec<_> = args.iter().map(|s| s.to_string_lossy()).collect();
        assert!(strings.contains(&"/NOCLOSEAPPLICATIONS".into()));
        assert!(strings.contains(&"/NOFORCECLOSEAPPLICATIONS".into()));
        assert!(strings.contains(&"/NORESTART".into()));
        assert!(!strings.iter().any(|s| s.starts_with("/TASKS")
            || s.starts_with("/RUN")
            || s == "/CLOSEAPPLICATIONS"));
    }
    #[test]
    fn registry_restore_cannot_touch_userchoice_hklm_or_foreign_values() {
        assert!(!registry_pair_allowed(
            "Software\\Classes\\.jpg\\UserChoice",
            "ProgId"
        ));
        assert!(!registry_pair_allowed(
            "Software\\RegisteredApplications",
            "foreign"
        ));
        assert!(registry_pair_allowed(
            "Software\\RegisteredApplications",
            "gloamere-image"
        ));
    }
}
