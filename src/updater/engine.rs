//! Real check/download/exit-helper bridge. Network authority comes only from the
//! compiled ProductionConfig and opaque TUF descriptors; disk JSON is expectation.
use super::{Result, UpdateError, client::Authorization, storage};
use safeupdate_core::{
    CancellationToken, Compatibility, LockedState, ProductionConfig, VerifiedTarget,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};
fn security(error: safeupdate_core::Error) -> UpdateError {
    UpdateError::Security(error.to_string())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUpdate {
    pub schema: u32,
    pub release_id: u64,
    pub version: String,
    pub target: String,
    pub sha256: String,
    pub length: u64,
    pub authorization: Authorization,
}
impl PendingUpdate {
    pub fn from_verified(target: &VerifiedTarget, authorization: Authorization) -> Self {
        Self {
            schema: 1,
            release_id: target.release().release_id,
            version: target.release().version.clone(),
            target: target.target_name().into(),
            sha256: hex(target.sha256()),
            length: target.length(),
            authorization,
        }
    }
    fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.release_id == 0
            || self.version.len() > 64
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.length == 0
            || self.length > safeupdate_core::APP_PACKAGE_LIMIT
        {
            return Err(UpdateError::Security("待更新描述无效".into()));
        }
        Ok(())
    }
    pub fn package_path(&self, root: &Path) -> Result<PathBuf> {
        self.validate()?;
        storage::owned_join(&root.join("downloads"), &format!("{}.exe", self.sha256))
    }
    pub fn matches(&self, target: &VerifiedTarget) -> bool {
        self.release_id == target.release().release_id
            && self.version == target.release().version
            && self.target == target.target_name()
            && self.sha256 == hex(target.sha256())
            && self.length == target.length()
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn ensure_private_directory(root: &Path) -> Result<()> {
    let _pins = super::platform::ensure_update_directory(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    super::platform::verify_current_user_path(root)?;
    Ok(())
}
pub fn open_trust(root: &Path, config: &ProductionConfig) -> Result<LockedState> {
    ensure_private_directory(root)?;
    let path = root.join("trust");
    let marker = root.join("trust-provisioned.json");
    let mut state = if path.exists() || marker.exists() {
        LockedState::open(&path, config).map_err(security)?
    } else {
        let state = LockedState::initialize(&path, config).map_err(security)?;
        storage::atomic_json(
            &marker,
            &serde_json::json!({"schema":1,"origin":config.origin(),"platform":config.platform(),"channel":config.channel()}),
        )?;
        state
    };
    let failed: Vec<u64> = match storage::read_json(&root.join("transactions/failed-releases.json"))
    {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if failed.len() > 1024 {
        return Err(UpdateError::Security("失败发行隔离记录过大".into()));
    }
    for id in failed {
        state.quarantine_app_release(id).map_err(security)?;
    }
    Ok(state)
}
pub fn compatibility(root: &Path) -> Result<Compatibility> {
    let installed: Option<super::recovery::InstalledState> =
        match storage::read_json(&root.join("transactions/installed.state")) {
            Ok(v) => Some(v),
            Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
    if installed
        .as_ref()
        .is_some_and(|s| s.schema != 1 || s.product_id != super::PRODUCT_ID)
    {
        return Err(UpdateError::Security("安装状态身份无效".into()));
    }
    let local = root.join("installation-id.json");
    let installation_id: String = match storage::read_json(&local) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            let temporary = tempfile::Builder::new()
                .prefix("rollout-")
                .rand_bytes(16)
                .tempfile_in(root)?;
            let id = temporary
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            storage::atomic_json(&local, &id)?;
            id
        }
        Err(e) => return Err(e),
    };
    if installation_id.len() > 128 || installation_id.is_empty() {
        return Err(UpdateError::Security("本地灰度种子无效".into()));
    }
    Ok(Compatibility {
        installed_release_id: installed.map_or(0, |s| s.release_id),
        updater_protocol: super::UPDATER_PROTOCOL,
        launcher_protocol: super::LAUNCHER_PROTOCOL,
        settings_schema: 1,
        ai_worker_protocol: crate::ai::model::WORKER_PROTOCOL,
        installation_id,
    })
}
pub enum JobResult {
    Checked(Option<Box<VerifiedTarget>>),
    Downloaded(PendingUpdate),
}
pub struct Job {
    pub cancel: CancellationToken,
    publication: Arc<Mutex<bool>>,
    pub receiver: mpsc::Receiver<Result<JobResult>>,
    thread: Option<JoinHandle<()>>,
}
impl Job {
    pub fn check(root: PathBuf, config: ProductionConfig) -> Self {
        Self::spawn(move |cancel, _gate| {
            let mut state = open_trust(&root, &config)?;
            let compatibility = compatibility(&root)?;
            let target =
                safeupdate_core::blocking_check(&config, &mut state, &compatibility, &cancel)
                    .map_err(security)?;
            Ok(JobResult::Checked(target.map(Box::new)))
        })
    }
    pub fn download(
        root: PathBuf,
        config: ProductionConfig,
        target: VerifiedTarget,
        authorization: Authorization,
    ) -> Self {
        Self::spawn(move |cancel, gate| {
            ensure_private_directory(&root.join("downloads"))?;
            let pending = PendingUpdate::from_verified(&target, authorization);
            let path = pending.package_path(&root)?;
            let mut state = open_trust(&root, &config)?;
            safeupdate_core::revalidate_for_execution(&config, &target, &mut state)
                .map_err(security)?;
            // A separate ownership lock spans receipt publication, completed-file
            // reuse, the downloader's inner lock, validation and pending publication.
            let _task = storage::LockGuard::acquire(&path.with_extension("exe.task.lock"), false)?;
            let task_path = path.with_extension("exe.task.json");
            if task_path.exists() {
                let old: PendingUpdate = storage::read_json(&task_path)?;
                old.validate()?;
                if old.sha256 != pending.sha256 || old.length != pending.length {
                    return Err(UpdateError::Security("下载任务记录冲突".into()));
                }
            }
            if let Ok(old) = storage::read_json::<PendingUpdate>(&root.join("pending.json")) {
                old.validate()?;
                if old.release_id == pending.release_id
                    && old.sha256 == pending.sha256
                    && old.authorization == Authorization::ExplicitApp
                    && pending.authorization == Authorization::AutomaticApp
                {
                    return Err(UpdateError::Deferred(
                        "手动授权的同一更新不被后台任务替换".into(),
                    ));
                }
            }
            storage::atomic_json(&task_path, &pending)?;
            let downloaded = if path.exists() {
                let (length, hash) = storage::hash_file(&path)?;
                if length != pending.length || hash != pending.sha256 {
                    return Err(UpdateError::Security(
                        "已有下载文件身份冲突，保留未知文件".into(),
                    ));
                }
                safeupdate_core::download::VerifiedDownload {
                    path: path.clone(),
                    length,
                    sha256: *target.sha256(),
                    resumed_from: length,
                }
            } else {
                safeupdate_core::blocking_download(&config, &target, &path, &cancel)
                    .map_err(security)?
            };
            if cancel.is_cancelled() {
                return Err(UpdateError::Deferred("下载已取消".into()));
            }
            if downloaded.length != pending.length || hex(&downloaded.sha256) != pending.sha256 {
                return Err(UpdateError::Security("下载结果身份不一致".into()));
            }
            // Publisher verification happens before 'ready', and once again at execution.
            let _lease =
                super::platform::ExecutionLease::verify(&path, &config, &target, &mut state)?;
            publish_pending(&root, &pending, &cancel, &gate)?;
            Ok(JobResult::Downloaded(pending))
        })
    }
    fn spawn(
        operation: impl FnOnce(CancellationToken, Arc<Mutex<bool>>) -> Result<JobResult>
        + Send
        + 'static,
    ) -> Self {
        let cancel = CancellationToken::new();
        let worker = cancel.clone();
        let publication = Arc::new(Mutex::new(false));
        let gate = publication.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(worker, gate)))
                    .unwrap_or_else(|_| {
                        Err(UpdateError::Security("更新后台异常，未自动安装".into()))
                    });
            let _ = tx.send(result);
        });
        Self {
            cancel,
            publication,
            receiver,
            thread: Some(thread),
        }
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Ok(mut gate) = self.publication.lock() {
            *gate = true;
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn publish_pending(
    root: &Path,
    pending: &PendingUpdate,
    cancel: &CancellationToken,
    gate: &Mutex<bool>,
) -> Result<()> {
    let cancelled = gate
        .lock()
        .map_err(|_| UpdateError::Security("下载提交门禁异常".into()))?;
    if *cancelled || cancel.is_cancelled() {
        return Err(UpdateError::Deferred("下载已取消".into()));
    }
    let _pending_lock = storage::LockGuard::acquire(&root.join("pending.lock"), false)?;
    if root.join("pending.json").exists() {
        let old: PendingUpdate = storage::read_json(&root.join("pending.json"))?;
        old.validate()?;
        if old.release_id > pending.release_id
            || (old.release_id == pending.release_id
                && (old.sha256 != pending.sha256
                    || old.length != pending.length
                    || (old.authorization == Authorization::ExplicitApp
                        && pending.authorization == Authorization::AutomaticApp)))
        {
            return Err(UpdateError::Security(
                "较新的待更新任务已就绪，旧任务不能覆盖".into(),
            ));
        }
    }
    storage::atomic_json(&root.join("pending.json"), pending)
}
pub fn cancel_download(root: &Path, expected: &PendingUpdate) -> Result<()> {
    let path = expected.package_path(root)?;
    let _task = storage::LockGuard::acquire(&path.with_extension("exe.task.lock"), false)?;
    let _download = storage::LockGuard::acquire(&path.with_extension("exe.download.lock"), false)?;
    let task_path = path.with_extension("exe.task.json");
    let task: PendingUpdate = match storage::read_json(&task_path) {
        Ok(t) => t,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return cancel_pending_locked(root, expected);
        }
        Err(e) => return Err(e),
    };
    task.validate()?;
    if &task != expected {
        return Ok(());
    }
    for suffix in ["exe.part", "exe.part.json"] {
        let file = path.with_extension(suffix);
        storage::plain_path(&file)?;
        if file.exists() {
            if !file.is_file() {
                return Err(UpdateError::Security("临时下载不是普通文件".into()));
            }
            std::fs::remove_file(file)?;
        }
    }
    if path.exists() {
        let (length, hash) = storage::hash_file(&path)?;
        if length != expected.length || hash != expected.sha256 {
            return Err(UpdateError::Security("已有包被改变，保留现场".into()));
        }
        std::fs::remove_file(&path)?;
    }
    std::fs::remove_file(task_path)?;
    cancel_pending_locked(root, expected)
}
pub fn cancel_pending(root: &Path, expected: Option<&PendingUpdate>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let path = expected.package_path(root)?;
    let _task = storage::LockGuard::acquire(&path.with_extension("exe.task.lock"), false)?;
    let _download = storage::LockGuard::acquire(&path.with_extension("exe.download.lock"), false)?;
    cancel_pending_locked(root, expected)
}
fn cancel_pending_locked(root: &Path, expected: &PendingUpdate) -> Result<()> {
    let _pending_lock = storage::LockGuard::acquire(&root.join("pending.lock"), false)?;
    let pending: PendingUpdate = match storage::read_json(&root.join("pending.json")) {
        Ok(p) => p,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    pending.validate()?;
    if &pending != expected {
        return Ok(());
    }
    let path = pending.package_path(root)?;
    // Delete only an exact verified product package. Changed/unknown data remains.
    if path.exists() {
        let (length, hash) = storage::hash_file(&path)?;
        if length != pending.length || hash != pending.sha256 {
            return Err(UpdateError::Security("暂存包已改变，未清理未知文件".into()));
        }
        std::fs::remove_file(path)?;
    }
    std::fs::remove_file(root.join("pending.json"))?;
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitTicket {
    pub schema: u32,
    pub nonce: String,
    pub process: u32,
    pub release_id: u64,
    pub normal: bool,
}
pub fn record_normal_exit(root: &Path, nonce: &str, process: u32) -> Result<()> {
    if nonce.is_empty() || nonce.len() > 128 {
        return Err(UpdateError::Security("退出握手无效".into()));
    }
    let pending: PendingUpdate = storage::read_json(&root.join("pending.json"))?;
    pending.validate()?;
    let preferences =
        super::client::Preferences::load(&root.parent().unwrap().join("update-settings.json"))?;
    match pending.authorization {
        Authorization::AutomaticApp if !preferences.automatic_app_updates => {
            return Err(UpdateError::Deferred("自动更新已关闭".into()));
        }
        Authorization::AutomaticInstalledModel => {
            return Err(UpdateError::Security("模型不能触发 EXE 安装".into()));
        }
        _ => {}
    }
    storage::atomic_json(
        &root.join("exit-ticket.json"),
        &ExitTicket {
            schema: 1,
            nonce: nonce.into(),
            process,
            release_id: pending.release_id,
            normal: true,
        },
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionGrant {
    pub schema: u32,
    pub installation: PathBuf,
    pub pending: PendingUpdate,
    pub previous_version: String,
}
pub fn spawn_after_normal_exit(
    root: &Path,
    installation: &Path,
    nonce: &str,
    child_pid: u32,
) -> Result<()> {
    if ProductionConfig::bundled().is_err() {
        return Ok(());
    }
    let ticket: ExitTicket = match storage::read_json(&root.join("exit-ticket.json")) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if ticket.schema != 1 || !ticket.normal || ticket.nonce != nonce || ticket.process != child_pid
    {
        return Err(UpdateError::Security(
            "退出凭据不属于刚结束的 UI 进程".into(),
        ));
    }
    let pending: PendingUpdate = storage::read_json(&root.join("pending.json"))?;
    pending.validate()?;
    if pending.release_id != ticket.release_id {
        return Err(UpdateError::Deferred(
            "待更新目标改变，需要再次正常退出".into(),
        ));
    }
    let launcher = installation.join(if cfg!(windows) {
        "gloamere-image.exe"
    } else {
        "gloamere-image"
    });
    let identity = super::recovery::FileIdentity::read(&launcher)?;
    let directory = root.join("helpers");
    ensure_private_directory(&directory)?;
    let helper = directory.join(format!("launcher-{}.exe", identity.sha256));
    if helper.exists() {
        if !identity.matches(&helper)? {
            return Err(UpdateError::Security("外置 helper 已变化".into()));
        }
    } else {
        let mut temporary = super::platform::private_tempfile(&directory)?;
        std::io::copy(&mut std::fs::File::open(&launcher)?, &mut temporary)?;
        temporary.as_file().sync_all()?;
        if !identity.matches(temporary.path())? {
            return Err(UpdateError::Security("外置 helper 复制校验失败".into()));
        }
        storage::persist_new(temporary, &helper)?;
    }
    super::platform::spawn_exit_helper(&helper, installation, nonce)
}
pub fn run_helper(args: Vec<std::ffi::OsString>) -> Result<i32> {
    let mut pairs = std::collections::BTreeMap::new();
    let (chunks, remainder) = args.as_chunks::<2>();
    for pair in chunks {
        let key = pair[0]
            .to_str()
            .ok_or_else(|| UpdateError::Security("helper 参数无效".into()))?;
        if !matches!(
            key,
            "--install-dir" | "--parent-pid" | "--parent-created" | "--nonce"
        ) || pairs.insert(key, &pair[1]).is_some()
        {
            return Err(UpdateError::Security("未知或重复 helper 参数".into()));
        }
    }
    if !remainder.is_empty() || pairs.len() != 4 {
        return Err(UpdateError::Security("helper 参数缺失".into()));
    }
    let installation = PathBuf::from(pairs["--install-dir"]);
    storage::plain_path(&installation)?;
    let pid = pairs["--parent-pid"]
        .to_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| UpdateError::Security("父进程参数无效".into()))?;
    let creation = pairs["--parent-created"]
        .to_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| UpdateError::Security("进程创建身份无效".into()))?;
    let nonce = pairs["--nonce"]
        .to_str()
        .ok_or_else(|| UpdateError::Security("退出握手无效".into()))?;
    super::platform::wait_exit_parent(pid, creation, &installation, nonce)?;
    let config = ProductionConfig::bundled().map_err(security)?;
    let root = super::launch::state_directory()
        .ok_or_else(|| UpdateError::Security("当前用户更新目录不可用".into()))?;
    let maintenance = storage::LockGuard::acquire(&root.join("maintenance.lock"), false)?;
    let sessions = storage::LockGuard::acquire(&root.join("sessions.lock"), false)?;
    let pending_lock = storage::LockGuard::acquire(&root.join("pending.lock"), false)?;
    let pending: PendingUpdate = storage::read_json(&root.join("pending.json"))?;
    pending.validate()?;
    let ticket: ExitTicket = storage::read_json(&root.join("exit-ticket.json"))?;
    if ticket.schema != 1
        || !ticket.normal
        || ticket.nonce != nonce
        || ticket.release_id != pending.release_id
    {
        return Err(UpdateError::Security("过期或不匹配的退出任务".into()));
    }
    super::platform::verify_current_user_path(&installation)?;
    if std::fs::read_to_string(installation.join("LIGHT-IMAGE-INSTALL.txt"))? != super::PRODUCT_ID {
        return Err(UpdateError::Security("安装目录归属不匹配".into()));
    }
    let preferences =
        super::client::Preferences::load(&root.parent().unwrap().join("update-settings.json"))?;
    let resources = super::platform::download_resources(&root, false)?;
    if resources.on_battery_low || resources.energy_saver {
        return Err(UpdateError::Deferred(
            "电量或节能状态不适合安装，保留已验证包".into(),
        ));
    }
    if resources.metered && !preferences.allow_metered_metadata {
        return Err(UpdateError::Deferred(
            "退出安装需要再次核验可信元数据，等待非计费网络或明确的元数据授权".into(),
        ));
    }
    let mut trust = open_trust(&root, &config)?;
    let compatibility = compatibility(&root)?;
    let target = safeupdate_core::blocking_check(
        &config,
        &mut trust,
        &compatibility,
        &CancellationToken::new(),
    )
    .map_err(security)?
    .ok_or_else(|| UpdateError::Deferred("待安装发行已不再适用，未执行包".into()))?;
    if !pending.matches(&target) {
        return Err(UpdateError::Deferred(
            "可信发行已改变，请重新检查更新".into(),
        ));
    }
    let lease = super::platform::ExecutionLease::verify(
        &pending.package_path(&root)?,
        &config,
        &target,
        &mut trust,
    )?;
    let preferences =
        super::client::Preferences::load(&root.parent().unwrap().join("update-settings.json"))?;
    if pending.authorization == Authorization::AutomaticInstalledModel
        || (pending.authorization == Authorization::AutomaticApp
            && !preferences.automatic_app_updates)
    {
        return Err(UpdateError::Deferred("退出安装授权已撤销".into()));
    }
    let previous_version = storage::read_json::<super::recovery::InstalledState>(
        &root.join("transactions/installed.state"),
    )
    .map(|s| s.version)
    .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
    let grant = ExecutionGrant {
        schema: 1,
        installation: installation.clone(),
        pending: pending.clone(),
        previous_version,
    };
    storage::atomic_json(&root.join("execution-grant.json"), &grant)?;
    let log = root.join("last-install.log");
    // Setup owns the same locks for its whole transaction. A competitor in this
    // narrow handoff wins and Setup refuses before writes; never force-close it.
    drop(pending_lock);
    drop(sessions);
    drop(maintenance);
    let code = lease.install(&config, &target, &mut trust, &installation, &log)?;
    drop(trust);
    super::installer_native::record_setup_exit(&installation, code)?;
    if code != 0 {
        return Err(UpdateError::Repair(format!(
            "安装器返回 {code}，未声明更新成功"
        )));
    }
    Ok(0)
}
pub fn reconcile_native_outcome(root: &Path, installation: &Path) -> Result<()> {
    let Some(outcome) = super::installer_native::last_outcome(installation)? else {
        return Ok(());
    };
    let grant: ExecutionGrant = match storage::read_json(&root.join("execution-grant.json")) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if grant.schema != 1 || grant.installation != installation {
        return Err(UpdateError::Security("安装发行凭据范围不匹配".into()));
    }
    grant.pending.validate()?;
    let (ticket, committed) = match outcome {
        super::installer_native::NativeOutcome::Committed(t) => (t, true),
        super::installer_native::NativeOutcome::RolledBack(t) => (t, false),
    };
    if ticket.package_sha256 != grant.pending.sha256 || ticket.version != grant.pending.version {
        return Ok(());
    } // An unrelated manual repair is not this release.
    ensure_private_directory(&root.join("transactions"))?;
    let failed_path = root.join("transactions/failed-releases.json");
    let mut failed: Vec<u64> = match storage::read_json(&failed_path) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if !committed {
        if !failed.contains(&grant.pending.release_id) {
            failed.push(grant.pending.release_id);
        }
        storage::atomic_json(&failed_path, &failed)?;
        if let Ok(config) = ProductionConfig::bundled() {
            let mut trust = open_trust(root, &config)?;
            trust
                .quarantine_app_release(grant.pending.release_id)
                .map_err(security)?;
        }
    } else {
        let state_path = root.join("transactions/installed.state");
        if state_path.exists() {
            let current: super::recovery::InstalledState = storage::read_json(&state_path)?;
            if current.release_id > grant.pending.release_id {
                return Err(UpdateError::Security("已安装发行序号不能回退".into()));
            }
        }
        let state = super::recovery::InstalledState {
            schema: 1,
            product_id: super::PRODUCT_ID.into(),
            version: grant.pending.version.clone(),
            previous_healthy_version: grant.previous_version.clone(),
            release_id: grant.pending.release_id,
            package_sha256: grant.pending.sha256.clone(),
            failed_releases: failed,
        };
        storage::atomic_json(&state_path, &state)?;
    }
    // Installed/quarantine state above is authoritative. Cache cleanup is best
    // effort and cannot turn a healthy commit into a rollback or startup error.
    if let Err(error) = clear_completed_pending(root, &grant.pending) {
        log::warn!("保留待更新记录，稍后重试清理：{error}");
    }
    if committed && let Err(error) = retire_completed_download(root, &grant.pending) {
        log::warn!("健康更新完成；保留无法安全清理的旧安装包：{error}");
    }
    Ok(())
}

fn clear_completed_pending(root: &Path, expected: &PendingUpdate) -> Result<()> {
    let _pending = storage::LockGuard::acquire(&root.join("pending.lock"), false)?;
    let pending = match storage::read_json::<PendingUpdate>(&root.join("pending.json")) {
        Ok(p) => p,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    pending.validate()?;
    expected.validate()?;
    if &pending == expected {
        std::fs::remove_file(root.join("pending.json"))?;
        storage::sync_directory(root)?;
    }
    Ok(())
}

/// A task receipt remains until its exact completed package is removed, making
/// interruption after file removal safely retryable. Failed releases retain the
/// package as evidence; only confirmed healthy native outcomes call this.
fn retire_completed_download(root: &Path, expected: &PendingUpdate) -> Result<()> {
    let path = expected.package_path(root)?;
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Security("下载路径无效".into()))?;
    if !parent.exists() {
        return Ok(());
    }
    super::platform::verify_current_user_path(parent)?;
    let _pins = super::platform::pin_directories(parent)?;
    let _task = storage::LockGuard::acquire(&path.with_extension("exe.task.lock"), false)?;
    let _download = storage::LockGuard::acquire(&path.with_extension("exe.download.lock"), false)?;
    let task_path = path.with_extension("exe.task.json");
    let task = match storage::read_json::<PendingUpdate>(&task_path) {
        Ok(task) => task,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if &task != expected {
        return Err(UpdateError::Security("下载清理记录不匹配，保留文件".into()));
    }
    for suffix in ["exe.part", "exe.part.json"] {
        let partial = path.with_extension(suffix);
        storage::plain_path(&partial)?;
        if partial.exists() {
            return Err(UpdateError::Deferred(
                "仍有可恢复下载片段，保留安装包".into(),
            ));
        }
    }
    storage::plain_path(&path)?;
    if path.exists() {
        if storage::hash_file(&path)? != (expected.length, expected.sha256.clone()) {
            return Err(UpdateError::Security(
                "健康安装包路径出现外来内容，保留现场".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        storage::sync_directory(parent)?;
    }
    std::fs::remove_file(task_path)?;
    storage::sync_directory(parent)
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::updater::test_support::{self, fs};
    fn fixture() -> (tempfile::TempDir, PendingUpdate) {
        let directory = test_support::tempdir().unwrap();
        ensure_private_directory(&directory.path().join("downloads")).unwrap();
        let bytes = b"owned completed installer";
        use sha2::Digest;
        let pending = PendingUpdate {
            schema: 1,
            release_id: 2,
            version: "0.7.0".into(),
            target: "stable/setup.exe".into(),
            sha256: hex(&sha2::Sha256::digest(bytes)),
            length: bytes.len() as u64,
            authorization: Authorization::ExplicitApp,
        };
        let path = pending.package_path(directory.path()).unwrap();
        fs::write(&path, bytes).unwrap();
        storage::atomic_json(&path.with_extension("exe.task.json"), &pending).unwrap();
        (directory, pending)
    }
    #[test]
    fn healthy_package_cleanup_is_exact_idempotent_and_keeps_new_pending() {
        let (d, old) = fixture();
        let root = d.path();
        let path = old.package_path(root).unwrap();
        let mut new = old.clone();
        new.release_id = 3;
        new.sha256 = "a".repeat(64);
        storage::atomic_json(&root.join("pending.json"), &new).unwrap();
        fs::write(root.join("downloads/foreign.txt"), b"foreign").unwrap();
        clear_completed_pending(root, &old).unwrap();
        retire_completed_download(root, &old).unwrap();
        retire_completed_download(root, &old).unwrap();
        assert!(!path.exists());
        assert_eq!(
            storage::read_json::<PendingUpdate>(&root.join("pending.json")).unwrap(),
            new
        );
        assert_eq!(
            fs::read(root.join("downloads/foreign.txt")).unwrap(),
            b"foreign"
        );
    }
    #[test]
    fn cleanup_resumes_after_package_removal_but_preserves_modified_bytes() {
        let (d, p) = fixture();
        let path = p.package_path(d.path()).unwrap();
        fs::write(&path, b"foreign changed bytes").unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign changed bytes");
        fs::remove_file(&path).unwrap();
        retire_completed_download(d.path(), &p).unwrap();
        assert!(!path.with_extension("exe.task.json").exists());
    }
    #[test]
    fn active_download_or_partial_blocks_completed_package_cleanup() {
        let (d, p) = fixture();
        let path = p.package_path(d.path()).unwrap();
        let lock =
            storage::LockGuard::acquire(&path.with_extension("exe.download.lock"), false).unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        drop(lock);
        fs::write(path.with_extension("exe.part"), b"partial").unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        assert!(path.exists());
    }
    #[test]
    fn pending_cleanup_preserves_unknown_schema_and_different_authorization() {
        let (d, p) = fixture();
        let root = d.path();
        let mut foreign = p.clone();
        foreign.schema = 2;
        storage::atomic_json(&root.join("pending.json"), &foreign).unwrap();
        assert!(clear_completed_pending(root, &p).is_err());
        assert_eq!(
            storage::read_json::<PendingUpdate>(&root.join("pending.json")).unwrap(),
            foreign
        );
        foreign = p.clone();
        foreign.authorization = Authorization::AutomaticApp;
        storage::atomic_json(&root.join("pending.json"), &foreign).unwrap();
        clear_completed_pending(root, &p).unwrap();
        assert_eq!(
            storage::read_json::<PendingUpdate>(&root.join("pending.json")).unwrap(),
            foreign
        );
    }
    #[test]
    fn task_ownership_lock_and_changed_receipt_block_retirement_and_cancellation() {
        let (d, p) = fixture();
        let path = p.package_path(d.path()).unwrap();
        let lock =
            storage::LockGuard::acquire(&path.with_extension("exe.task.lock"), false).unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        assert!(cancel_download(d.path(), &p).is_err());
        drop(lock);
        let mut next = p.clone();
        next.authorization = Authorization::AutomaticApp;
        storage::atomic_json(&path.with_extension("exe.task.json"), &next).unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        cancel_download(d.path(), &p).unwrap();
        assert!(path.exists());
        assert_eq!(
            storage::read_json::<PendingUpdate>(&path.with_extension("exe.task.json")).unwrap(),
            next
        );
    }
    #[cfg(unix)]
    #[test]
    fn package_symlink_is_never_followed_for_cleanup() {
        let (d, p) = fixture();
        let path = p.package_path(d.path()).unwrap();
        fs::remove_file(&path).unwrap();
        let foreign = d.path().join("foreign");
        fs::write(&foreign, b"safe").unwrap();
        std::os::unix::fs::symlink(&foreign, &path).unwrap();
        assert!(retire_completed_download(d.path(), &p).is_err());
        assert_eq!(fs::read(foreign).unwrap(), b"safe");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::test_support::{self, fs};
    #[test]
    fn no_builtin_root_can_authorize_network_or_execution() {
        assert!(ProductionConfig::bundled().is_err());
    }
    #[test]
    fn fake_pending_target_never_becomes_verified_authority() {
        let p = PendingUpdate {
            schema: 1,
            release_id: 7,
            version: "0.7.1".into(),
            target: "ignored".into(),
            sha256: "a".repeat(64),
            length: 4,
            authorization: Authorization::AutomaticApp,
        };
        let d = test_support::tempdir().unwrap();
        assert!(
            p.package_path(d.path())
                .unwrap()
                .starts_with(d.path().join("downloads"))
        );
        assert!(ProductionConfig::bundled().is_err());
    }
    #[test]
    fn cancel_preserves_foreign_files_and_changed_package() {
        let d = test_support::tempdir().unwrap();
        fs::create_dir(d.path().join("downloads")).unwrap();
        let p = PendingUpdate {
            schema: 1,
            release_id: 7,
            version: "0.7.1".into(),
            target: "x".into(),
            sha256: "a".repeat(64),
            length: 4,
            authorization: Authorization::ExplicitApp,
        };
        storage::atomic_json(&d.path().join("pending.json"), &p).unwrap();
        fs::write(p.package_path(d.path()).unwrap(), b"else").unwrap();
        assert!(cancel_pending(d.path(), Some(&p)).is_err());
        assert!(d.path().join("pending.json").exists());
    }
}
