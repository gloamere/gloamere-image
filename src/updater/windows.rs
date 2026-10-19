#![cfg_attr(test, allow(dead_code))] // Native registry/reboot adapters are isolated from unit fixtures.
use super::*;
use std::{
    ffi::OsStr,
    io::{Seek, SeekFrom},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, WinTrust::*, *},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
    },
    System::{Registry::*, Threading::*},
};
fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
fn os_error() -> UpdateError {
    std::io::Error::last_os_error().into()
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Local(*mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub fn file_sddl(path: &Path) -> Result<String> {
    let path = wide(path);
    let mut descriptor = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    let _descriptor = Local(descriptor);
    let mut text = ptr::null_mut();
    let mut count = 0;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            &mut count,
        )
    } == 0
    {
        return Err(os_error());
    }
    let _text = Local(text.cast());
    if count > 65536 {
        return Err(UpdateError::Security("安全描述符过大".into()));
    }
    let chars = unsafe { std::slice::from_raw_parts(text, count as usize) };
    Ok(String::from_utf16_lossy(chars)
        .trim_end_matches('\0')
        .to_owned())
}
// Handle-based capture pairs with the pinned leaf setter; renamed ancestors cannot redirect it.
pub fn handle_sddl(file: &std::fs::File) -> Result<String> {
    let mut descriptor = ptr::null_mut();
    let code = unsafe {
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
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    let _descriptor = Local(descriptor);
    let mut text = ptr::null_mut();
    let mut count = 0;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            &mut count,
        )
    } == 0
    {
        return Err(os_error());
    }
    let _text = Local(text.cast());
    if count > 65536 {
        return Err(UpdateError::Security(
            "Security descriptor too large".into(),
        ));
    }
    let chars = unsafe { std::slice::from_raw_parts(text, count as usize) };
    Ok(String::from_utf16_lossy(chars)
        .trim_end_matches('\0')
        .to_owned())
}
pub fn restore_sddl(path: &Path, sddl: &str) -> Result<()> {
    if sddl.len() > 65536 {
        return Err(UpdateError::Security("安全描述符过大".into()));
    }
    verify_owner(path)?;
    let current = file_sddl(path)?;
    if current == sddl {
        return Ok(());
    }
    // A directory setter propagates ACEs to unrelated descendants. Never mutate it.
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(UpdateError::Repair(
            "Changed directory or reparse descriptor; retain snapshot".into(),
        ));
    }
    let encoded = wide(sddl);
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            encoded.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    let _owned = Local(descriptor);
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(os_error());
    }
    // Modern setters add AI. Do not mutate a legacy preimage we cannot restore exactly.
    if control & SE_DACL_AUTO_INHERITED == 0 {
        return Err(UpdateError::Repair(
            "Changed legacy descriptor needs manual repair; retain snapshot".into(),
        ));
    }
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ) // Pin the leaf against deletion/rename.
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT) // Never follow a replacement.
        .open(path)?;
    let pinned = file.metadata()?;
    if !pinned.is_file() || pinned.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(UpdateError::Repair(
            "Pinned target is not a plain file; retain snapshot".into(),
        ));
    }
    if handle_sddl(&file)? != current {
        return Err(UpdateError::Repair(
            "File descriptor changed before restoration; retain snapshot".into(),
        ));
    }
    let inheritance = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(UpdateError::Repair(
            "Missing original DACL; retain snapshot".into(),
        ));
    }
    let result = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | inheritance,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result as i32).into());
    }
    if handle_sddl(&file)? != sddl {
        return Err(UpdateError::Repair(
            "恢复后文件安全描述符不一致；保留修复证据".into(),
        ));
    }
    Ok(())
}
pub fn verify_owner(path: &Path) -> Result<()> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_error());
    }
    let _token = Handle(token);
    let mut length = 0;
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length) };
    if length == 0 || length > 65536 {
        return Err(UpdateError::Security("不能查询当前用户".into()));
    }
    // usize storage maintains TOKEN_USER alignment.
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
        return Err(os_error());
    }
    let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    let mut owner = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let path = wide(path);
    let result = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result as i32).into());
    }
    let _descriptor = Local(descriptor);
    if unsafe { EqualSid(owner, user.User.Sid) } == 0 {
        return Err(UpdateError::PathOwnerMismatch);
    }
    if dacl.is_null() {
        return Err(UpdateError::PathUnsafeWriteAccess);
    }
    let mut system = [0u32; 17];
    let mut admins = [0u32; 17];
    let mut size = 68u32;
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            ptr::null_mut(),
            system.as_mut_ptr().cast(),
            &mut size,
        )
    } == 0
    {
        return Err(os_error());
    }
    size = 68;
    if unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            ptr::null_mut(),
            admins.as_mut_ptr().cast(),
            &mut size,
        )
    } == 0
    {
        return Err(os_error());
    }
    let write_mask =
        0x2u32 | 0x4 | 0x10 | 0x40 | 0x100 | 0x10000 | 0x40000 | 0x80000 | 0x10000000 | 0x40000000;
    for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType == 1 {
            continue;
        } // A deny entry does not expand access.
        if !matches!(header.AceType, 0 | 4 | 5 | 9 | 11) {
            return Err(UpdateError::Security(
                "不支持的 DACL 类型，不能证明更新目录安全".into(),
            ));
        }
        if header.AceSize < 8 {
            return Err(UpdateError::Security("DACL 条目无效".into()));
        }
        let mask = unsafe { *(ace.cast::<u8>().add(4).cast::<u32>()) };
        if mask & write_mask == 0 {
            continue;
        }
        if header.AceType != 0 || header.AceSize < 12 {
            return Err(UpdateError::Security("不支持的可写 DACL 条目".into()));
        }
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = ptr::addr_of!(allowed.SidStart).cast_mut().cast();
        if unsafe { IsValidSid(sid) } == 0 {
            return Err(UpdateError::Security("DACL SID 无效".into()));
        }
        if unsafe { EqualSid(sid, user.User.Sid) } == 0
            && unsafe { EqualSid(sid, system.as_mut_ptr().cast()) } == 0
            && unsafe { EqualSid(sid, admins.as_mut_ptr().cast()) } == 0
        {
            return Err(UpdateError::PathUnsafeWriteAccess);
        }
    }
    Ok(())
}
pub fn verify_package(
    path: &Path,
    identity: &super::super::recovery::FileIdentity,
    policy: &PublisherPolicy,
) -> Result<ExecutionLease> {
    let directories = pin_directories(
        path.parent()
            .ok_or_else(|| UpdateError::Security("包父路径无效".into()))?,
    )?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)?;
    let (length, hash) = storage::hash_reader(&mut file)?;
    if identity.length != length || identity.sha256 != hash {
        return Err(UpdateError::Security("执行前包身份不符".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    let encoded = wide(path);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: encoded.as_ptr(),
        hFile: file.as_raw_handle(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut trust = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe { WinVerifyTrustEx(ptr::null_mut(), &mut action, &mut trust) };
    let verification = (|| {
        if status == TRUST_E_NOSIGNATURE {
            if !matches!(policy, PublisherPolicy::TufAuthenticatedUnsigned) {
                return Err(UpdateError::Security(
                    "需要指定发布者的 Authenticode 签名".into(),
                ));
            }
            // Explicitly unsigned means PE security directory is absent. A broken
            // embedded signature must not be downgraded to 'unsigned'.
            if pe_has_signature(&mut file)? {
                return Err(UpdateError::Security(
                    "存在无法验证的签名，不能按未签名包执行".into(),
                ));
            }
            return Ok(PublisherVerification::UnsignedTufAuthenticated);
        }
        if status != 0 {
            return Err(UpdateError::Security(format!(
                "Windows 拒绝包签名或证书：{status:#x}"
            )));
        }
        let provider = unsafe { WTHelperProvDataFromStateData(trust.hWVTStateData) };
        if provider.is_null() {
            return Err(UpdateError::Security("无法读取发布者链".into()));
        }
        let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 0, 0) };
        if signer.is_null() {
            return Err(UpdateError::Security("发布者链缺少签名者".into()));
        }
        let cert = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
        if cert.is_null() || unsafe { (*cert).pCert }.is_null() {
            return Err(UpdateError::Security("发布者证书缺失".into()));
        }
        let context = unsafe { &*(*cert).pCert };
        if context.cbCertEncoded > 1024 * 1024 {
            return Err(UpdateError::Security("证书过大".into()));
        }
        let der = unsafe {
            std::slice::from_raw_parts(context.pbCertEncoded, context.cbCertEncoded as usize)
        };
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(der));
        match policy {
            PublisherPolicy::RequireCertificate { sha256 } if sha256 == &hash => {
                Ok(PublisherVerification::Authenticode {
                    certificate_sha256: hash,
                })
            }
            _ => Err(UpdateError::Security(
                "现有签名不符合预置发布者策略，不能通过远端配置放行".into(),
            )),
        }
    })();
    trust.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrustEx(ptr::null_mut(), &mut action, &mut trust);
    }
    let verification = verification?;
    file.seek(SeekFrom::Start(0))?;
    let (length, hash) = storage::hash_reader(&mut file)?;
    if identity.length != length || identity.sha256 != hash {
        return Err(UpdateError::Security("签名校验期间包被改变".into()));
    }
    Ok(ExecutionLease {
        directories,
        file,
        path: path.to_path_buf(),
        verification,
    })
}
fn pe_has_signature(file: &mut File) -> Result<bool> {
    use std::io::Read;
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header)?;
    if &header[..2] != b"MZ" {
        return Err(UpdateError::Security("不是 PE 文件".into()));
    }
    let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
    if offset > 16 * 1024 * 1024 {
        return Err(UpdateError::Security("PE 头偏移异常".into()));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut nt = [0u8; 26];
    file.read_exact(&mut nt)?;
    if &nt[..4] != b"PE\0\0" {
        return Err(UpdateError::Security("PE 头无效".into()));
    }
    let magic = u16::from_le_bytes(nt[24..26].try_into().unwrap());
    let directories = match magic {
        0x10b => 96,
        0x20b => 112,
        _ => return Err(UpdateError::Security("PE 类型无效".into())),
    };
    let optional_size = u16::from_le_bytes(nt[20..22].try_into().unwrap()) as u64;
    if optional_size < directories + 40 {
        return Err(UpdateError::Security("PE 签名目录缺失".into()));
    }
    file.seek(SeekFrom::Start(offset + 24 + directories + 32))?;
    let mut security = [0u8; 8];
    file.read_exact(&mut security)?;
    Ok(security != [0; 8])
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn read_registry(root: HKEY, key: &str, name: &str) -> Result<(u32, Option<Vec<u8>>)> {
    let mut handle = ptr::null_mut();
    let code = unsafe {
        RegOpenKeyExW(
            root,
            wide(key).as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &mut handle,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok((0, None));
    }
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    let _key = Key(handle);
    let mut kind = 0;
    let mut size = 0;
    let code = unsafe {
        RegQueryValueExW(
            handle,
            wide(name).as_ptr(),
            ptr::null(),
            &mut kind,
            ptr::null_mut(),
            &mut size,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok((0, None));
    }
    if code != 0 || size > 65536 {
        return Err(UpdateError::Security("登记快照无法读取或过大".into()));
    }
    let mut bytes = vec![0u8; size as usize];
    let code = unsafe {
        RegQueryValueExW(
            handle,
            wide(name).as_ptr(),
            ptr::null(),
            &mut kind,
            bytes.as_mut_ptr(),
            &mut size,
        )
    };
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    bytes.truncate(size as usize);
    Ok((kind, Some(bytes)))
}
pub fn capture_integration(installation: &Path) -> Result<IntegrationSnapshot> {
    verify_owner(installation)?;
    let mut registry = Vec::new();
    for (key, name) in registry_pairs() {
        let (kind, bytes) = read_registry(HKEY_CURRENT_USER, &key, &name)?;
        registry.push(RegistryValue {
            key,
            name,
            kind,
            bytes,
        });
    }
    let snapshot = IntegrationSnapshot { registry };
    snapshot.validate(installation)?;
    Ok(snapshot)
}
pub fn restore_integration(snapshot: &IntegrationSnapshot, installation: &Path) -> Result<()> {
    verify_owner(installation)?;
    // Do not overwrite concurrent/unknown registry mutations. Inno's persisted
    // journal records its exact postimages; when these cannot be established,
    // fail into repair rather than guessing that every difference is ours.
    for value in &snapshot.registry {
        let (kind, bytes) = read_registry(HKEY_CURRENT_USER, &value.key, &value.name)?;
        if bytes != value.bytes || (bytes.is_some() && kind != value.kind) {
            return Err(UpdateError::Repair(
                "产品登记已变化；缺少可信安装后值，保留快照并需要修复安装登记".into(),
            ));
        }
    }
    Ok(())
}
pub fn reboot_operations_clear() -> Result<bool> {
    let (_, bytes) = read_registry(
        HKEY_LOCAL_MACHINE,
        "SYSTEM\\CurrentControlSet\\Control\\Session Manager",
        "PendingFileRenameOperations",
    )?;
    Ok(bytes.is_none_or(|b| b.iter().all(|x| *x == 0)))
}

pub fn metered_network() -> bool {
    use windows_sys::Win32::{Networking::WinSock::*, System::LibraryLoader::*};
    // Optional dynamic entry point keeps pre-2004 Windows 10 startup working.
    let library = unsafe {
        LoadLibraryExW(
            wide("iphlpapi.dll").as_ptr(),
            ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if library.is_null() {
        return true;
    }
    let entry = unsafe { GetProcAddress(library, c"GetNetworkConnectivityHint".as_ptr().cast()) };
    let metered = if let Some(entry) = entry {
        type GetHint = unsafe extern "system" fn(*mut NL_NETWORK_CONNECTIVITY_HINT) -> u32;
        let get_hint: GetHint = unsafe { std::mem::transmute(entry) };
        let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
        (unsafe { get_hint(&mut hint) }) != 0
            || hint.ConnectivityCost != NetworkConnectivityCostHintUnrestricted
            || hint.ApproachingDataLimit
            || hint.OverDataLimit
            || hint.Roaming
    } else {
        true
    };
    unsafe {
        FreeLibrary(library);
    }
    metered
}
pub fn resources(root: &Path, heavy_work: bool) -> Result<super::super::client::Resources> {
    use windows_sys::Win32::{Storage::FileSystem::GetDiskFreeSpaceExW, System::Power::*};
    let existing = if root.exists() {
        root
    } else {
        root.parent()
            .ok_or_else(|| UpdateError::Security("更新卷无效".into()))?
    };
    let mut download_free = 0;
    if unsafe {
        GetDiskFreeSpaceExW(
            wide(existing).as_ptr(),
            &mut download_free,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    let exe = std::env::current_exe()?;
    let installation = exe.parent().unwrap();
    let mut install_free = 0;
    if unsafe {
        GetDiskFreeSpaceExW(
            wide(installation).as_ptr(),
            &mut install_free,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    let mut power: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut power) } == 0 {
        return Err(os_error());
    }
    Ok(super::super::client::Resources {
        metered: metered_network(),
        on_battery_low: power.ACLineStatus != 1
            && (power.BatteryFlag & 6 != 0 || power.BatteryLifePercent < 20),
        energy_saver: power.SystemStatusFlag != 0,
        heavy_work,
        download_free,
        install_free,
    })
}
unsafe extern "system" fn shutdown_observer(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    use windows_sys::Win32::UI::{Shell::*, WindowsAndMessaging::*};
    if message == WM_QUERYENDSESSION || message == WM_ENDSESSION {
        super::SESSION_ENDING.store(true, std::sync::atomic::Ordering::Release);
    }
    if message == WM_NCDESTROY {
        unsafe {
            RemoveWindowSubclass(hwnd, Some(shutdown_observer), id);
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
pub fn observe_shutdown(hwnd: HWND) -> Result<()> {
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    if unsafe { SetWindowSubclass(hwnd, Some(shutdown_observer), 0x474c4f41, 0) } == 0 {
        return Err(os_error());
    }
    Ok(())
}

fn process_proof(handle: HANDLE) -> Result<(u64, PathBuf)> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(os_error());
    }
    let mut text = vec![0u16; 32768];
    let mut length = text.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, text.as_mut_ptr(), &mut length) } == 0 {
        return Err(os_error());
    }
    let path = PathBuf::from(String::from_utf16_lossy(&text[..length as usize]));
    storage::plain_path(&path)?;
    Ok((
        (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        path,
    ))
}
fn update_ready_event(nonce: &str) -> Result<Vec<u16>> {
    if nonce.len() > 128
        || nonce.is_empty()
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(UpdateError::Security("更新握手标识无效".into()));
    }
    Ok(wide(format!("Local\\GloamereUpdateReady-{nonce}")))
}
pub fn spawn_exit_helper(helper: &Path, installation: &Path, nonce: &str) -> Result<()> {
    let (created, _) = process_proof(unsafe { GetCurrentProcess() })?;
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, update_ready_event(nonce)?.as_ptr()) };
    if event.is_null() {
        return Err(os_error());
    }
    let _event = Handle(event);
    let mut command = std::process::Command::new(helper);
    command
        .arg("--update-helper")
        .arg("--install-dir")
        .arg(installation)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--parent-created")
        .arg(created.to_string())
        .arg("--nonce")
        .arg(nonce);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    let mut child = command.spawn()?;
    if unsafe { WaitForSingleObject(event, 30000) } != WAIT_OBJECT_0 {
        return Err(UpdateError::Deferred(format!(
            "更新 helper 未确认持有父进程句柄；未执行安装，子进程状态 {:?}",
            child.try_wait()?
        )));
    }
    Ok(())
}
pub fn wait_exit_parent(pid: u32, creation: u64, installation: &Path, nonce: &str) -> Result<()> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(os_error());
    }
    let _snapshot = Handle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut found = unsafe { Process32FirstW(snapshot, &mut entry) };
    let mut actual_parent = None;
    while found != 0 {
        if entry.th32ProcessID == std::process::id() {
            actual_parent = Some(entry.th32ParentProcessID);
            break;
        }
        found = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    if actual_parent != Some(pid) {
        return Err(UpdateError::Security(
            "更新 helper 的实际父进程不匹配".into(),
        ));
    }
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return Err(os_error());
    }
    let _process = Handle(process);
    let (actual_created, image) = process_proof(process)?;
    if actual_created != creation || image != installation.join("gloamere-image.exe") {
        return Err(UpdateError::Security(
            "父进程身份、创建时间或安装范围不符".into(),
        ));
    }
    verify_owner(installation)?;
    if storage::hash_file(&image)? != storage::hash_file(&std::env::current_exe()?)? {
        return Err(UpdateError::Security(
            "外置 helper 与稳定入口不是同一程序".into(),
        ));
    }
    let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, update_ready_event(nonce)?.as_ptr()) };
    if event.is_null() {
        return Err(os_error());
    }
    let _event = Handle(event);
    if unsafe { SetEvent(event) } == 0 {
        return Err(os_error());
    }
    if unsafe { WaitForSingleObject(process, 120000) } != WAIT_OBJECT_0 {
        return Err(UpdateError::Deferred(
            "稳定入口尚未退出，本次不安装；没有结束任何进程".into(),
        ));
    }
    Ok(())
}

pub fn pin_directories(path: &Path) -> Result<Vec<File>> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    let mut handles = Vec::new();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            // Directory 0x2/0x4 aliases require deny-write lifetime sharing too.
            // Attribute-only writers are handled by the caller's security audit.
            .share_mode(1)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(ancestor)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(UpdateError::Security("目录句柄指向重解析点或非目录".into()));
        }
        handles.push(file);
    }
    Ok(handles)
}

pub fn current_user_update_root() -> Result<PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
    };
    let mut text = ptr::null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, ptr::null_mut(), &mut text) };
    if result < 0 || text.is_null() {
        return Err(UpdateError::Security(
            "不能解析当前用户 LocalAppData".into(),
        ));
    }
    let mut length = 0;
    unsafe {
        while length < 32768 && *text.add(length) != 0 {
            length += 1;
        }
    }
    let path = if length < 32768 {
        Ok(PathBuf::from(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, length)
        }))
        .join("LightImageTool/updater-state"))
    } else {
        Err(UpdateError::Security("用户目录路径过长".into()))
    };
    unsafe {
        CoTaskMemFree(text.cast());
    }
    path
}

#[cfg(test)]
mod directory_descriptor_tests {
    use super::*;
    fn legacy_write(path: &Path, sddl: &str) {
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
        let _owned = Local(descriptor);
        assert_ne!(
            unsafe { SetFileSecurityW(wide(path).as_ptr(), DACL_SECURITY_INFORMATION, descriptor) },
            0
        );
    }
    fn fixture() -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("unrelated-folder");
        std::fs::create_dir(&nested).unwrap();
        let a = root.path().join("unrelated.txt");
        let b = nested.join("nested.txt");
        std::fs::write(&a, b"SYNTHETIC").unwrap();
        std::fs::write(&b, b"SYNTHETIC").unwrap();
        let paths = vec![root.path().to_owned(), nested, a, b];
        for leaf in &paths[2..] {
            legacy_write(leaf, &file_sddl(leaf).unwrap());
        }
        (root, paths)
    }
    #[test]
    fn unchanged_directory_never_rewrites_unrelated_descendant_dacls() {
        let (_root, paths) = fixture();
        let before: Vec<_> = paths.iter().map(|p| file_sddl(p).unwrap()).collect();
        assert!(before[0].contains("AI"));
        assert!(!before[2].contains("AI"));
        let permissions = crate::updater::platform::capture_permissions(&paths[0]).unwrap();
        crate::updater::platform::restore_permissions(&paths[0], &permissions).unwrap();
        let after: Vec<_> = paths.iter().map(|p| file_sddl(p).unwrap()).collect();
        assert_eq!(after, before);
    }
    #[test]
    fn changed_directory_fails_before_any_parent_or_descendant_mutation() {
        let (_root, paths) = fixture();
        let original = file_sddl(&paths[0]).unwrap();
        legacy_write(&paths[0], &original);
        let before: Vec<_> = paths.iter().map(|p| file_sddl(p).unwrap()).collect();
        assert_ne!(before[0], original);
        assert!(matches!(
            restore_sddl(&paths[0], &original),
            Err(UpdateError::Repair(_))
        ));
        let after: Vec<_> = paths.iter().map(|p| file_sddl(p).unwrap()).collect();
        assert_eq!(after, before);
    }
    #[test]
    fn changed_inherited_leaf_restores_exactly_using_pinned_handle() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("owned.txt");
        std::fs::write(&leaf, b"SYNTHETIC").unwrap();
        let original = file_sddl(&leaf).unwrap();
        assert!(original.contains("AI"));
        legacy_write(&leaf, &original);
        let legacy = file_sddl(&leaf).unwrap();
        assert_ne!(legacy, original);
        restore_sddl(&leaf, &original).unwrap();
        assert_eq!(file_sddl(&leaf).unwrap(), original);
        assert!(matches!(
            restore_sddl(&leaf, &legacy),
            Err(UpdateError::Repair(_))
        ));
        assert_eq!(file_sddl(&leaf).unwrap(), original);
        restore_sddl(&leaf, &original).unwrap();
    }
}
