//! Stable entry point and UI health handshake. No background service or process killing.
use super::{
    Result, UpdateError,
    recovery::{HealthReport, Phase, Transaction},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
pub fn state_directory() -> Option<PathBuf> {
    super::platform::current_user_update_root().ok()
}
pub fn runtime_path(installation: &Path) -> PathBuf {
    installation.join("runtime").join(if cfg!(windows) {
        "gloamere-image-ui.exe"
    } else {
        "gloamere-image-ui"
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveTransaction {
    pub schema: u32,
    pub transaction: String,
    pub installation: PathBuf,
}
pub fn active_transaction(state: &Path, installation: &Path) -> Result<Option<Transaction>> {
    let active: ActiveTransaction = match storage::read_json(&state.join("active.json")) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if active.schema != 1 || active.installation != installation || active.transaction.contains('/')
    {
        return Err(UpdateError::Security("活动更新事务归属不匹配".into()));
    }
    let directory = storage::owned_join(&state.join("transactions"), &active.transaction)?;
    let t = Transaction::open(directory)?;
    if t.journal.installation != installation {
        return Err(UpdateError::Security("更新事务路径不一致".into()));
    }
    Ok(Some(t))
}
pub fn run(args: Vec<std::ffi::OsString>) -> Result<i32> {
    if args.first().is_some_and(|a| a == "--updater-protocol") {
        println!("{}", super::LAUNCHER_PROTOCOL);
        return Ok(0);
    }
    if args.first().is_some_and(|a| a == "--installer-transaction") {
        return super::installer_native::run(args.into_iter().skip(1).collect());
    }
    if args.first().is_some_and(|a| a == "--update-helper") {
        return super::engine::run_helper(args.into_iter().skip(1).collect());
    }
    let exe = std::env::current_exe()?;
    let installation = exe
        .parent()
        .ok_or_else(|| UpdateError::Security("启动路径无效".into()))?;
    storage::plain_path(installation)?;
    let state = state_directory()
        .ok_or_else(|| UpdateError::Repair("不能定位当前用户更新状态目录".into()))?;
    let _state_pins = super::platform::ensure_update_directory(&state)?;
    let launch_admission = storage::LockGuard::wait_for(
        &state.join("launch-admission.lock"),
        false,
        Duration::from_secs(120),
    )?;
    let maintenance = storage::LockGuard::wait_for(
        &state.join("maintenance.lock"),
        false,
        Duration::from_secs(120),
    )?;
    #[cfg(windows)]
    let startup = super::installer_native::recover_before_launch_locked(installation)?;
    #[cfg(not(windows))]
    let startup = super::installer_native::Startup::Ready;
    let health = match startup {
        super::installer_native::Startup::Ready => None,
        super::installer_native::Startup::PendingHealth(ticket) => Some(ticket),
        super::installer_native::Startup::RecoverySpawned => return Ok(0),
    };
    // Runtime existence comes after recovery, which may restore a missing UI.
    let runtime = runtime_path(installation);
    if !runtime.is_file() {
        return Err(UpdateError::Repair("缺少 runtime/gloamere-image-ui；请使用完整引导 EXE 修复。源码目录请运行 gloamere-image-ui。".into()));
    }
    let session = storage::LockGuard::acquire(&state.join("sessions.lock"), true)?;
    let nonce_file = tempfile::Builder::new()
        .prefix("launch-")
        .rand_bytes(16)
        .tempfile_in(&state)?;
    let nonce = nonce_file
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    drop(nonce_file);
    let mut command = Command::new(&runtime);
    command.args(&args).env("GLOAMERE_LAUNCH_NONCE", &nonce);
    let health_path = if let Some(ticket) = &health {
        let directory = state.join("health");
        let _health_directory = super::platform::ensure_update_directory(&directory)?;
        let path = storage::owned_join(&directory, &format!("{}.json", ticket.transaction))?;
        if path.exists() {
            return Err(UpdateError::Repair(
                "已有健康凭据不能用于新的启动，保留诊断".into(),
            ));
        }
        command
            .env("GLOAMERE_HEALTH_TRANSACTION", &ticket.transaction)
            .env("GLOAMERE_HEALTH_PACKAGE", &ticket.package_sha256)
            .env("GLOAMERE_HEALTH_PARENT", std::process::id().to_string());
        Some(path)
    } else {
        None
    };
    drop(maintenance); // Shared session lock excludes Setup throughout child startup.
    #[cfg(windows)]
    super::engine::reconcile_native_outcome(&state, installation)?;
    let mut child = command.spawn()?;
    let child_pid = child.id();
    if let (Some(path), Some(ticket)) = (health_path.as_ref(), health.as_ref()) {
        let started = Instant::now();
        let mut healthy = false;
        loop {
            if let Ok(handshake) = storage::read_json::<Handshake>(path)
                && handshake.transaction == ticket.transaction
                && handshake.parent == std::process::id()
                && handshake.process == child_pid
                && handshake.nonce == nonce
            {
                match super::installer_native::confirm_health(
                    installation,
                    ticket,
                    &handshake.report,
                ) {
                    Ok(()) => {
                        super::engine::reconcile_native_outcome(&state, installation)?;
                        healthy = true;
                    }
                    Err(error) => log::error!("健康确认失败：{error}"),
                }
                break;
            }
            if child.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(30) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !healthy {
            // Wait on this exact child, never kill a UI that may hold user work.
            let status = child.wait()?;
            drop(session);
            let _ = super::installer_native::fail_health(installation, "新版 UI 未完成健康握手")?;
            super::engine::reconcile_native_outcome(&state, installation)?;
            return Ok(status.code().unwrap_or(1));
        }
    }
    drop(launch_admission);
    let status = child.wait()?;
    drop(session);
    if status.success()
        && let Err(error) =
            super::engine::spawn_after_normal_exit(&state, installation, &nonce, child_pid)
    {
        log::warn!("退出更新暂缓：{error}");
    }
    Ok(status.code().unwrap_or(1))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub transaction: String,
    pub nonce: String,
    pub parent: u32,
    pub process: u32,
    pub report: HealthReport,
}
pub struct RuntimeGuard {
    _session: storage::LockGuard,
    _state_directory: super::platform::StateDirectoryGuard,
}
impl RuntimeGuard {
    pub fn acquire() -> Result<Option<Self>> {
        let exe = std::env::current_exe()?;
        let runtime = exe
            .parent()
            .ok_or_else(|| UpdateError::Security("应用路径无效".into()))?;
        if runtime.file_name().and_then(|s| s.to_str()) != Some("runtime") {
            return Ok(None);
        } // Development binaries have no installed update protocol.
        let installation = runtime
            .parent()
            .ok_or_else(|| UpdateError::Security("应用安装路径无效".into()))?;
        let state =
            state_directory().ok_or_else(|| UpdateError::Security("用户状态目录无效".into()))?;
        let state_pins = super::platform::ensure_update_directory(&state)?;
        let maintenance = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
        #[cfg(windows)]
        super::installer_native::runtime_admission(
            installation,
            std::env::var("GLOAMERE_HEALTH_TRANSACTION").ok().as_deref(),
        )?;
        if let Some(t) = active_transaction(&state, installation)? {
            let permitted = t.journal.phase == Phase::PendingHealth
                && std::env::var("GLOAMERE_HEALTH_TRANSACTION").ok().as_deref()
                    == Some(&t.journal.transaction);
            if !permitted
                && !matches!(
                    t.journal.phase,
                    Phase::Committed | Phase::RolledBack | Phase::Prepared
                )
            {
                return Err(UpdateError::Deferred(
                    "请通过 gloamere-image 稳定入口等待更新恢复".into(),
                ));
            }
        }
        let session = storage::LockGuard::acquire(&state.join("sessions.lock"), true)?;
        drop(maintenance);
        Ok(Some(Self {
            _session: session,
            _state_directory: state_pins,
        }))
    }
}
pub struct HealthProbe {
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for HealthProbe {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn start_ui_health(settings_readable: bool, ui_frames: u64) -> Option<HealthProbe> {
    std::env::var_os("GLOAMERE_HEALTH_TRANSACTION")?;
    Some(HealthProbe {
        thread: Some(std::thread::spawn(move || {
            if let Err(error) = report_ui_health(settings_readable, ui_frames) {
                log::error!("健康握手失败：{error}");
            }
        })),
    })
}
fn required_worker_health() -> Result<()> {
    use std::io::Read;
    let executable = std::env::current_exe()?;
    let worker = executable.parent().unwrap().join(if cfg!(windows) {
        "gloamere-image-worker.exe"
    } else {
        "gloamere-image-worker"
    });
    storage::plain_path(&worker)?;
    let mut command = Command::new(worker);
    command
        .arg("--health-protocol")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(UpdateError::Repair("必需来源 worker 自检失败".into()));
            }
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UpdateError::Repair("必需来源 worker 自检超时".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut bytes = Vec::new();
    if let Some(out) = child.stdout.take() {
        out.take(4097).read_to_end(&mut bytes)?;
    }
    if bytes.len() > 4096 {
        return Err(UpdateError::Security("worker 自检输出过大".into()));
    }
    let result: serde_json::Value = serde_json::from_slice(&bytes)?;
    if result.get("protocol").and_then(|v| v.as_u64()) != Some(1)
        || result.get("version").and_then(|v| v.as_str()) != Some(env!("CARGO_PKG_VERSION"))
        || result.get("offline").and_then(|v| v.as_bool()) != Some(true)
    {
        return Err(UpdateError::Repair("必需 worker 版本或协议不匹配".into()));
    }
    Ok(())
}
/// Called only after actual UI frames and settings load. Does not read user images.
pub fn report_ui_health(settings_readable: bool, ui_frames: u64) -> Result<()> {
    let Ok(transaction) = std::env::var("GLOAMERE_HEALTH_TRANSACTION") else {
        return Ok(());
    };
    if ui_frames < 2 {
        return Ok(());
    }
    let package_sha256 = std::env::var("GLOAMERE_HEALTH_PACKAGE")
        .map_err(|_| UpdateError::Security("健康包身份缺失".into()))?;
    let parent = std::env::var("GLOAMERE_HEALTH_PARENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| UpdateError::Security("健康父进程缺失".into()))?;
    if package_sha256.len() != 64 || !package_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdateError::Security("健康包身份无效".into()));
    }
    let state =
        state_directory().ok_or_else(|| UpdateError::Security("健康状态目录缺失".into()))?;
    let health_path = storage::owned_join(&state.join("health"), &format!("{transaction}.json"))?;
    let nonce = std::env::var("GLOAMERE_LAUNCH_NONCE")
        .map_err(|_| UpdateError::Security("健康启动标识缺失".into()))?;
    let built_in_decode =
        image::load_from_memory(include_bytes!("../../assets/gloamere-image-16.png")).is_ok();
    required_worker_health()?;
    let report = HealthReport {
        version: env!("CARGO_PKG_VERSION").into(),
        package_sha256,
        updater_protocol: super::UPDATER_PROTOCOL,
        launcher_protocol: super::LAUNCHER_PROTOCOL,
        worker_protocol: crate::ai::model::WORKER_PROTOCOL,
        settings_readable,
        built_in_decode,
        ui_event_loop: true,
    };
    storage::atomic_json(
        &health_path,
        &Handshake {
            transaction,
            nonce,
            parent,
            process: std::process::id(),
            report,
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_launcher_keeps_shell_path_stable() {
        assert!(runtime_path(Path::new("/app")).ends_with(if cfg!(windows) {
            "runtime/gloamere-image-ui.exe"
        } else {
            "runtime/gloamere-image-ui"
        }));
    }
    #[test]
    fn invalid_active_transaction_never_traverses() {
        let d = tempfile::tempdir().unwrap();
        let active = ActiveTransaction {
            schema: 1,
            transaction: "../foreign".into(),
            installation: PathBuf::from("/app"),
        };
        storage::atomic_json(&d.path().join("active.json"), &active).unwrap();
        assert!(active_transaction(d.path(), Path::new("/app")).is_err());
    }
}
