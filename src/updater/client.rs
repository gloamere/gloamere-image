//! UI-independent consent, scheduling, exit and package state.
use super::{PRODUCTION_DISABLED_REASON, Result, UpdateError, storage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u8,
    pub automatic_app_updates: bool,
    pub automatic_installed_model_updates: bool,
    pub allow_metered_metadata: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            automatic_app_updates: false,
            automatic_installed_model_updates: false,
            allow_metered_metadata: false,
        }
    }
}
impl Preferences {
    pub fn load(path: &Path) -> Result<Self> {
        let p: Self = storage::read_json(path)?;
        if p.schema_version != 1 {
            return Err(UpdateError::Security("更新设置版本未知，保留原文件".into()));
        }
        Ok(p)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if self.schema_version != 1 {
            return Err(UpdateError::Security("不支持的设置版本".into()));
        }
        match Self::load(path) {
            Ok(_) => {}
            Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        storage::atomic_json(path, self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    AutomaticApp,
    ExplicitApp,
    AutomaticInstalledModel,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    Disabled(String),
    Checking,
    Available { version: String },
    Downloading { received: u64, total: u64 },
    Paused(String),
    Ready { version: String },
    WaitingExit(String),
    Installing,
    PendingHealth,
    NoEligibleUpdate,
    Committed(String),
    Cancelled,
    Failed(String),
}
impl State {
    pub fn text(&self) -> String {
        match self {
            Self::Idle => "尚未检查更新".into(),
            Self::Disabled(s) => s.clone(),
            Self::Checking => "正在检查可信发行信息".into(),
            Self::Available { version } => format!("发现版本 {version}，下载前需要确认"),
            Self::Downloading { received, total } => format!("正在下载：{received} / {total} 字节"),
            Self::Paused(s) => format!("下载已暂停：{s}"),
            Self::Ready { version } => format!("{version} 已校验，等待正常退出后安装"),
            Self::WaitingExit(s) => format!("等待安全退出：{s}"),
            Self::Installing => "正在安装，请等待完成或安全恢复".into(),
            Self::PendingHealth => "已安装，等待下次启动健康验证".into(),
            Self::NoEligibleUpdate => "可信仓库未发现适用于当前版本的新发行".into(),
            Self::Committed(v) => format!("已更新到 {v}"),
            Self::Cancelled => "本次更新已取消".into(),
            Self::Failed(s) => format!("更新未完成：{s}"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    NormalUserExit,
    ShutdownOrLogoff,
    Crash,
    WindowHidden,
    Idle,
}
#[derive(Debug, Clone, Default)]
pub struct ExitConditions {
    pub unsaved: bool,
    pub exporting: bool,
    pub modal: bool,
    pub workers_active: bool,
    pub other_instances: bool,
}
impl ExitConditions {
    pub fn permission(&self, reason: ExitReason) -> Result<()> {
        if reason != ExitReason::NormalUserExit {
            return Err(UpdateError::Deferred(
                "只有用户正常退出后才安装；关机、隐藏或空闲均不触发".into(),
            ));
        }
        if self.unsaved
            || self.exporting
            || self.modal
            || self.workers_active
            || self.other_instances
        {
            return Err(UpdateError::Deferred(
                "未保存工作、导出、对话框、worker 或其他实例尚未结束".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default)]
pub struct Resources {
    pub metered: bool,
    pub on_battery_low: bool,
    pub energy_saver: bool,
    pub heavy_work: bool,
    pub download_free: u64,
    pub install_free: u64,
}
impl Resources {
    pub fn transfer_permission(&self, allow_metered_once: bool) -> Result<()> {
        if self.metered && !allow_metered_once {
            return Err(UpdateError::Deferred("计费网络，需明确允许本次下载".into()));
        }
        if self.on_battery_low || self.energy_saver || self.heavy_work {
            return Err(UpdateError::Deferred("电量或系统负载暂不适合下载".into()));
        }
        Ok(())
    }
    pub fn download_permission(
        &self,
        allow_metered_once: bool,
        bytes: u64,
        expanded: u64,
        snapshot: u64,
    ) -> Result<()> {
        self.transfer_permission(allow_metered_once)?;
        let needed = expanded
            .checked_add(snapshot)
            .and_then(|v| v.checked_add(256 * 1024 * 1024))
            .ok_or_else(|| UpdateError::Security("空间计算溢出".into()))?;
        if self.download_free < bytes || self.install_free < needed {
            return Err(UpdateError::Deferred("磁盘空间不足，保留旧版".into()));
        }
        Ok(())
    }
}
pub struct Client {
    completion_notice: Option<String>,
    completion_checked: Option<u64>,
    pub allow_metered_once: bool,
    config: Option<safeupdate_core::ProductionConfig>,
    root: Option<PathBuf>,
    job: Option<super::engine::Job>,
    target: Option<safeupdate_core::VerifiedTarget>,
    pending: Option<super::engine::PendingUpdate>,
    check_was_automatic: bool,
    pub preferences: Preferences,
    pub state: State,
    pub settings_error: Option<String>,
    settings_path: Option<PathBuf>,
    authorization: Option<Authorization>,
    cancel_requested: bool,
    cancel_expected: Option<super::engine::PendingUpdate>,
    active_download: Option<super::engine::PendingUpdate>,
    next_check_secs: u64,
    retry_attempt: u8,
    resource_paused: bool,
    resource_checked: Option<u64>,
}
impl Default for Client {
    fn default() -> Self {
        Self {
            completion_notice: None,
            completion_checked: None,
            allow_metered_once: false,
            config: safeupdate_core::ProductionConfig::bundled().ok(),
            root: None,
            job: None,
            target: None,
            pending: None,
            check_was_automatic: false,
            preferences: Preferences::default(),
            state: State::Disabled(PRODUCTION_DISABLED_REASON.into()),
            settings_error: None,
            settings_path: None,
            authorization: None,
            cancel_requested: false,
            cancel_expected: None,
            active_download: None,
            next_check_secs: 60,
            retry_attempt: 0,
            resource_paused: false,
            resource_checked: None,
        }
    }
}
impl Client {
    pub fn load(path: Option<PathBuf>) -> Self {
        #[cfg(all(windows, not(test)))]
        let path = {
            let _ = path;
            super::launch::state_directory()
                .and_then(|p| p.parent().map(|p| p.join("update-settings.json")))
        };
        let root = path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.join("updater-state"));
        let mut this = Self {
            settings_path: path,
            root,
            ..Self::default()
        };
        if let Some(path) = &this.settings_path {
            match Preferences::load(path) {
                Ok(p) => this.preferences = p,
                Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => this.settings_error = Some(e.to_string()),
            }
        }
        if this.config.is_some() {
            this.state = State::Idle;
            if let Some(root) = &this.root
                && let Ok(pending) =
                    storage::read_json::<super::engine::PendingUpdate>(&root.join("pending.json"))
            {
                let authorized = match pending.authorization {
                    Authorization::AutomaticApp => this.preferences.automatic_app_updates,
                    Authorization::ExplicitApp => true,
                    Authorization::AutomaticInstalledModel => false,
                };
                if authorized {
                    this.state = State::Ready {
                        version: pending.version.clone(),
                    };
                    this.authorization = Some(pending.authorization);
                    this.pending = Some(pending);
                }
            }
        }
        this
    }
    fn save(&mut self) {
        if let Some(path) = &self.settings_path {
            self.settings_error = self.preferences.save(path).err().map(|e| e.to_string());
        }
    }
    pub fn set_automatic_app(&mut self, on: bool) {
        self.preferences.automatic_app_updates = on;
        if !on
            && (self.authorization == Some(Authorization::AutomaticApp)
                || (self.job.is_some() && self.check_was_automatic))
        {
            self.cancel();
        }
        self.save();
    }
    pub fn set_model_updates(&mut self, on: bool) {
        self.preferences.automatic_installed_model_updates = on;
        if !on && self.authorization == Some(Authorization::AutomaticInstalledModel) {
            self.cancel();
        }
        self.save();
    }
    pub fn set_metered_metadata(&mut self, on: bool) {
        self.preferences.allow_metered_metadata = on;
        self.save();
    }
    pub fn manual_check(&mut self) {
        if matches!(
            self.state,
            State::Ready { .. } | State::Installing | State::PendingHealth
        ) {
            return;
        }
        self.begin_check(false);
    }
    fn begin_check(&mut self, automatic: bool) {
        if self.job.is_some() {
            return;
        }
        let (Some(config), Some(root)) = (self.config.clone(), self.root.clone()) else {
            self.state = State::Disabled(PRODUCTION_DISABLED_REASON.into());
            return;
        };
        self.cancel_requested = false;
        self.check_was_automatic = automatic;
        self.allow_metered_once = false;
        self.state = State::Checking;
        self.job = Some(super::engine::Job::check(root, config));
    }
    pub fn poll(&mut self, elapsed_secs: u64, heavy_work: bool) {
        if self.resource_checked != Some(elapsed_secs) {
            self.resource_checked = Some(elapsed_secs);
            if matches!(self.state, State::Downloading { .. }) {
                if let Some(root) = &self.root {
                    let policy = super::platform::download_resources(root, heavy_work)
                        .and_then(|r| r.transfer_permission(self.allow_metered_once));
                    if let Err(error) = policy {
                        if let Some(job) = &self.job {
                            job.cancel();
                        }
                        self.state = State::Paused(format!("资源条件变化：{error}"));
                        self.resource_paused = true;
                    }
                }
            } else if self.resource_paused
                && self.job.is_none()
                && self.authorized()
                && let Some(authorization) = self.authorization
            {
                let _ = self.begin_download(authorization, heavy_work);
            }
        }

        if self.completion_checked != Some(elapsed_secs) {
            self.completion_checked = Some(elapsed_secs);
            self.check_completion_notice();
        }
        if let State::Downloading { received, total } = &mut self.state
            && let (Some(root), Some(target)) = (&self.root, &self.target)
        {
            let pending = super::engine::PendingUpdate::from_verified(
                target,
                self.authorization.unwrap_or(Authorization::ExplicitApp),
            );
            if let Ok(path) = pending.package_path(root) {
                let partial = path.with_extension("exe.part");
                *received = std::fs::metadata(&path)
                    .or_else(|_| std::fs::metadata(partial))
                    .map_or(0, |m| m.len().min(*total));
            }
        }
        if let Some(result) = self.job.as_ref().and_then(|j| j.receiver.try_recv().ok()) {
            self.job.take();
            if self.cancel_requested {
                self.cleanup_cancelled();
                return;
            }
            if matches!(self.state, State::Paused(_)) {
                return;
            }
            match result {
                Ok(super::engine::JobResult::Checked(Some(target))) => {
                    self.state = State::Available {
                        version: target.release().version.clone(),
                    };
                    self.target = Some(*target);
                    if self.check_was_automatic && self.preferences.automatic_app_updates {
                        let _ = self.begin_download(Authorization::AutomaticApp, heavy_work);
                    }
                }
                Ok(super::engine::JobResult::Checked(None)) => self.state = State::NoEligibleUpdate,
                Ok(super::engine::JobResult::Downloaded(pending)) => {
                    self.active_download = None;
                    self.state = State::Ready {
                        version: pending.version.clone(),
                    };
                    self.pending = Some(pending);
                }
                Err(error) => {
                    self.retry_attempt = self.retry_attempt.saturating_add(1).min(9);
                    self.next_check_secs =
                        elapsed_secs.saturating_add((30u64 << self.retry_attempt).min(4 * 60 * 60));
                    self.state = State::Failed(error.to_string());
                }
            }
        }
        if self.config.is_some()
            && self.job.is_none()
            && self.automatic_due(
                elapsed_secs,
                super::platform::network_is_metered_or_unknown(),
            )
            && matches!(
                self.state,
                State::Idle | State::Failed(_) | State::NoEligibleUpdate | State::Committed(_)
            )
        {
            let jitter = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| (d.subsec_nanos() % 3601) as u16);
            self.checked(elapsed_secs, jitter);
            self.begin_check(true);
        }
    }
    pub fn begin_download(&mut self, authorization: Authorization, heavy_work: bool) -> Result<()> {
        if self.job.is_some() {
            return Err(UpdateError::Deferred("已有更新任务".into()));
        }
        let config = self
            .config
            .clone()
            .ok_or_else(|| UpdateError::Deferred(PRODUCTION_DISABLED_REASON.into()))?;
        let root = self
            .root
            .clone()
            .ok_or_else(|| UpdateError::Security("更新目录不可用".into()))?;
        let target = self
            .target
            .clone()
            .ok_or_else(|| UpdateError::Deferred("请先检查更新".into()))?;
        self.authorize_download(authorization)?;
        self.active_download = Some(super::engine::PendingUpdate::from_verified(
            &target,
            authorization,
        ));
        let resources = super::platform::download_resources(&root, heavy_work)?;
        // Conservative hard reserve until exact authenticated native plan is
        // extracted. Native prepare checks actual expanded and snapshot sizes.
        if let Err(error) = resources.download_permission(
            self.allow_metered_once,
            target.length(),
            2 * 1024 * 1024 * 1024,
            512 * 1024 * 1024,
        ) {
            self.state = State::Paused(error.to_string());
            self.resource_paused = true;
            return Err(error);
        }
        self.resource_paused = false;
        self.state = State::Downloading {
            received: 0,
            total: target.length(),
        };
        self.job = Some(super::engine::Job::download(
            root,
            config,
            target,
            authorization,
        ));
        Ok(())
    }
    fn check_completion_notice(&mut self) {
        let Some(root) = &self.root else {
            return;
        };
        let Ok(installed) = storage::read_json::<super::recovery::InstalledState>(
            &root.join("transactions/installed.state"),
        ) else {
            return;
        };
        if installed.schema != 1
            || installed.product_id != super::PRODUCT_ID
            || installed.version != env!("CARGO_PKG_VERSION")
        {
            return;
        }
        let announcement = root.join("last-announced.json");
        if storage::read_json::<String>(&announcement).ok().as_deref()
            == Some(&installed.package_sha256)
        {
            return;
        }
        if storage::atomic_json(&announcement, &installed.package_sha256).is_ok() {
            self.completion_notice = Some(format!("已更新到 {}，启动验证通过", installed.version));
            if matches!(
                self.state,
                State::Idle | State::Disabled(_) | State::PendingHealth
            ) {
                self.state = State::Committed(installed.version);
            }
        }
    }
    pub fn take_completion_notice(&mut self) -> Option<String> {
        self.completion_notice.take()
    }
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }
    pub(crate) fn model_context(&self) -> Option<(safeupdate_core::ProductionConfig, PathBuf)> {
        Some((self.config.clone()?, self.root.clone()?))
    }
    pub fn is_configured(&self) -> bool {
        self.config.is_some()
    }
    pub fn record_exit(&mut self, conditions: &ExitConditions, reason: ExitReason) -> Result<()> {
        conditions.permission(reason)?;
        if !matches!(self.state, State::Ready { .. }) || !self.authorized() {
            return Ok(());
        }
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| UpdateError::Security("更新目录不可用".into()))?;
        let nonce = std::env::var("GLOAMERE_LAUNCH_NONCE")
            .map_err(|_| UpdateError::Deferred("下次通过稳定入口退出后安装".into()))?;
        super::engine::record_normal_exit(root, &nonce, std::process::id())
    }
    pub fn automatic_due(&self, elapsed_secs: u64, metered: bool) -> bool {
        self.preferences.automatic_app_updates
            && elapsed_secs >= self.next_check_secs
            && (!metered || self.preferences.allow_metered_metadata)
    }
    pub fn checked(&mut self, elapsed_secs: u64, jitter_secs: u16) {
        self.next_check_secs = elapsed_secs
            .saturating_add(24 * 60 * 60)
            .saturating_add(u64::from(jitter_secs.min(3600)));
    }
    pub fn authorize_download(&mut self, kind: Authorization) -> Result<()> {
        let allowed = match kind {
            Authorization::AutomaticApp => self.preferences.automatic_app_updates,
            Authorization::ExplicitApp => true,
            Authorization::AutomaticInstalledModel => {
                self.preferences.automatic_installed_model_updates
            }
        };
        if !allowed {
            return Err(UpdateError::Security("此更新任务没有相应授权".into()));
        }
        self.authorization = Some(kind);
        self.cancel_requested = false;
        Ok(())
    }
    pub fn is_waiting_for_resources(&self) -> bool {
        self.resource_paused
    }
    pub fn cancel(&mut self) {
        self.resource_paused = false;
        self.allow_metered_once = false;
        if let Some(job) = &self.job {
            job.cancel();
        }
        self.cancel_requested = true;
        self.cancel_expected = self
            .active_download
            .clone()
            .or_else(|| self.pending.clone());
        if !matches!(self.state, State::Installing | State::PendingHealth) {
            self.authorization = None;
            self.state = State::Cancelled;
            if self.job.is_none() {
                self.cleanup_cancelled();
            }
        }
    }
    fn cleanup_cancelled(&mut self) {
        if let Some(root) = &self.root {
            let result = if let Some(expected) = &self.cancel_expected {
                super::engine::cancel_download(root, expected)
            } else {
                Ok(())
            };
            if let Err(error) = result {
                self.settings_error = Some(error.to_string());
            } else {
                self.pending = None;
                self.active_download = None;
            }
        }
    }
    pub fn pause(&mut self) {
        self.resource_paused = false;
        if matches!(self.state, State::Downloading { .. }) {
            if let Some(job) = &self.job {
                job.cancel();
            }
            self.state = State::Paused("用户暂停，保留可续传临时包".into());
        }
    }
    pub fn authorized(&self) -> bool {
        self.authorization.is_some() && !self.cancel_requested
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_is_off_and_manual_check_is_not_download_consent() {
        let mut c = Client::default();
        assert!(!c.preferences.automatic_app_updates);
        assert!(!c.preferences.automatic_installed_model_updates);
        assert!(!c.automatic_due(90000, false));
        c.manual_check();
        assert!(!c.authorized());
        assert!(matches!(c.state, State::Disabled(_)));
    }
    #[test]
    fn app_consent_never_authorizes_model_and_off_cancels_ready() {
        let mut c = Client::default();
        c.set_automatic_app(true);
        c.authorize_download(Authorization::AutomaticApp).unwrap();
        assert!(
            c.authorize_download(Authorization::AutomaticInstalledModel)
                .is_err()
        );
        c.state = State::Ready {
            version: "0.7.1".into(),
        };
        c.set_automatic_app(false);
        assert!(!c.authorized());
        assert_eq!(c.state, State::Cancelled);
    }
    #[test]
    fn commit_cancellation_never_kills_or_claims_success() {
        let mut c = Client::default();
        c.authorize_download(Authorization::ExplicitApp).unwrap();
        c.state = State::Installing;
        c.cancel();
        assert_eq!(c.state, State::Installing);
        assert!(!c.authorized());
    }
    #[test]
    fn resource_changes_suspend_transfer_without_broadening_metered_consent() {
        let mut r = Resources::default();
        assert!(r.transfer_permission(false).is_ok());
        r.metered = true;
        assert!(r.transfer_permission(false).is_err());
        assert!(r.transfer_permission(true).is_ok());
        r.on_battery_low = true;
        assert!(r.transfer_permission(true).is_err());
        r.on_battery_low = false;
        r.energy_saver = true;
        assert!(r.transfer_permission(true).is_err());
        r.energy_saver = false;
        r.heavy_work = true;
        assert!(r.transfer_permission(true).is_err());
    }
    #[test]
    fn cancel_revokes_transfer_exception_and_manual_pause_disables_auto_resume() {
        let mut c = Client {
            allow_metered_once: true,
            ..Client::default()
        };
        c.cancel();
        assert!(!c.allow_metered_once);
        assert!(!c.resource_paused);
        c.resource_paused = true;
        c.pause();
        assert!(!c.resource_paused);
    }
    #[test]
    fn normal_exit_only_with_all_work_drained() {
        for reason in [
            ExitReason::Crash,
            ExitReason::Idle,
            ExitReason::ShutdownOrLogoff,
            ExitReason::WindowHidden,
        ] {
            assert!(ExitConditions::default().permission(reason).is_err());
        }
        for flag in 0..5 {
            let mut c = ExitConditions::default();
            match flag {
                0 => c.unsaved = true,
                1 => c.exporting = true,
                2 => c.modal = true,
                3 => c.workers_active = true,
                _ => c.other_instances = true,
            };
            assert!(c.permission(ExitReason::NormalUserExit).is_err());
        }
        assert!(
            ExitConditions::default()
                .permission(ExitReason::NormalUserExit)
                .is_ok()
        );
    }
    #[test]
    fn update_preferences_do_not_modify_v06_settings() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("settings.json");
        crate::settings::save(&old, &crate::settings::Preferences::default()).unwrap();
        let before = std::fs::read(&old).unwrap();
        let mut c = Client::load(Some(d.path().join("update-settings.json")));
        c.set_automatic_app(true);
        assert_eq!(std::fs::read(&old).unwrap(), before);
        assert!(
            Preferences::load(&d.path().join("update-settings.json"))
                .unwrap()
                .automatic_app_updates
        );
    }
    #[test]
    fn unknown_preferences_survive_toggles() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("update-settings.json");
        let old = b"{\"schema_version\":2,\"future\":true}";
        std::fs::write(&p, old).unwrap();
        let mut c = Client::load(Some(p.clone()));
        c.set_automatic_app(true);
        assert!(c.settings_error.is_some());
        assert_eq!(std::fs::read(p).unwrap(), old);
    }
    #[test]
    fn schedule_has_delay_metered_policy_and_bounded_jitter() {
        let mut c = Client::default();
        c.set_automatic_app(true);
        assert!(!c.automatic_due(59, false));
        assert!(c.automatic_due(60, false));
        assert!(!c.automatic_due(60, true));
        c.checked(60, 65535);
        assert!(!c.automatic_due(90059, false));
        assert!(c.automatic_due(90060, false));
    }
}
