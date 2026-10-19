//! Persistent exact-file transaction and recovery engine.
//! Platform integration is separately snapshotted and restored; user data is
//! outside the installation root and never appears in an owned file manifest.
use super::{PRODUCT_ID, Result, UpdateError, platform, storage};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};
const JOURNAL_SCHEMA: u32 = 1;
const MAX_FILES: usize = 4096;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebootSafety {
    Unknown,
    Clear,
    Pending,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Downloaded,
    Verified,
    Preparing,
    Prepared,
    Installing,
    PendingHealth,
    CommittingHealth,
    Committed,
    RollingBack,
    RolledBack,
    RepairRequired,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub length: u64,
    pub sha256: String,
}
impl FileIdentity {
    pub fn read(path: &Path) -> Result<Self> {
        let (length, sha256) = storage::hash_file(path)?;
        Ok(Self { length, sha256 })
    }
    fn validate(&self) -> Result<()> {
        if self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.length > 2 * 1024 * 1024 * 1024
        {
            return Err(UpdateError::Security("文件身份无效".into()));
        }
        Ok(())
    }
    pub fn matches(&self, path: &Path) -> Result<bool> {
        Ok(&Self::read(path)? == self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadFile {
    pub path: String,
    pub identity: FileIdentity,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSnapshot {
    pub path: String,
    pub before: Option<FileIdentity>,
    pub after: Option<FileIdentity>,
    pub permissions: Option<platform::FilePermissions>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub schema: u32,
    pub product_id: String,
    pub transaction: String,
    pub installation: PathBuf,
    pub previous_version: String,
    pub target_version: String,
    pub release_id: u64,
    pub package_identity: FileIdentity,
    pub phase: Phase,
    pub files: Vec<FileSnapshot>,
    pub integration: platform::IntegrationSnapshot,
    pub exit_code: Option<i32>,
    pub failure: Option<String>,
    pub health_attempted: bool,
    pub reboot_safety: RebootSafety,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledState {
    pub schema: u32,
    pub product_id: String,
    pub version: String,
    pub previous_healthy_version: String,
    pub release_id: u64,
    pub package_sha256: String,
    pub failed_releases: Vec<u64>,
}
#[derive(Debug, Clone)]
pub struct Plan {
    pub installation: PathBuf,
    pub previous_version: String,
    pub target_version: String,
    pub release_id: u64,
    pub package_identity: FileIdentity,
    pub installed_files: Vec<PayloadFile>,
    pub target_files: Vec<PayloadFile>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    pub version: String,
    pub package_sha256: String,
    pub updater_protocol: u32,
    pub launcher_protocol: u32,
    pub worker_protocol: u32,
    pub settings_readable: bool,
    pub built_in_decode: bool,
    pub ui_event_loop: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    JournalCreated,
    FileBackedUp(usize),
    Prepared,
    Installing,
    InstallerReturned,
    FileRestored(usize),
    PendingHealth,
    BeforeHealthCommit,
    HealthCommitted,
}
#[derive(Debug, Default)]
pub struct NoFault;
pub trait Faults {
    fn checkpoint(&mut self, stage: Checkpoint) -> Result<()>;
}
impl Faults for NoFault {
    fn checkpoint(&mut self, _: Checkpoint) -> Result<()> {
        Ok(())
    }
}
pub struct Transaction {
    directory: PathBuf,
    pub journal: Journal,
}
fn failure(text: &str) -> UpdateError {
    UpdateError::Security(text.into())
}
fn validate_payload(files: &[PayloadFile]) -> Result<()> {
    if files.len() > MAX_FILES {
        return Err(failure("文件清单超出范围"));
    }
    let mut paths = BTreeSet::new();
    for f in files {
        storage::relative_path(&f.path)?;
        f.identity.validate()?;
        if !paths.insert(f.path.to_lowercase()) {
            return Err(failure("文件清单有大小写冲突"));
        }
        if !owned_payload_path(&f.path) {
            return Err(failure("清单含非产品拥有路径"));
        }
    }
    Ok(())
}
pub fn owned_payload_path(path: &str) -> bool {
    matches!(
        path,
        "runtime/gloamere-image-ui.exe"
            | "runtime/gloamere-image-worker.exe"
            | "runtime/gloamere-image-ui"
            | "runtime/gloamere-image-worker"
            | "assets/gloamere-image.ico"
            | "README.md"
            | "LICENSE-MIT"
            | "LICENSE-APACHE"
            | "VALIDATION.md"
            | "MANIFEST.json"
            | "LIGHT-IMAGE-INSTALL.txt"
    ) || path.starts_with("third-party-notices/")
        || (path.len() == 12
            && path.starts_with("unins")
            && path.as_bytes()[5..8].iter().all(|v| v.is_ascii_digit())
            && matches!(path.get(8..), Some(".exe" | ".dat" | ".msg")))
}
impl Transaction {
    /// Caller must hold the exclusive installation/session lock, and supply file
    /// identities bound to the authenticated package. This method never downloads
    /// or executes untrusted manifests, commands or scripts.
    pub fn prepare(directory: PathBuf, plan: Plan, fault: &mut impl Faults) -> Result<Self> {
        storage::plain_path(&directory)?;
        storage::plain_path(&plan.installation)?;
        validate_payload(&plan.installed_files)?;
        validate_payload(&plan.target_files)?;
        plan.package_identity.validate()?;
        if directory.starts_with(&plan.installation) {
            return Err(failure("恢复快照必须在安装目录外"));
        }
        if directory.exists() {
            return Err(failure("事务目录已存在，先恢复不能覆盖"));
        }
        let marker = storage::owned_join(&plan.installation, "LIGHT-IMAGE-INSTALL.txt")?;
        if fs::read_to_string(marker)? != PRODUCT_ID {
            return Err(failure("安装所有权标记不匹配"));
        }
        platform::verify_current_user_path(&plan.installation)?;
        let mut paths = BTreeSet::new();
        for f in plan.installed_files.iter().chain(&plan.target_files) {
            paths.insert(f.path.clone());
        }
        if paths.len() > MAX_FILES {
            return Err(failure("合并清单过大"));
        }
        let mut files = Vec::new();
        for path in paths {
            let live = storage::owned_join(&plan.installation, &path)?;
            let declared = plan.installed_files.iter().find(|f| f.path == path);
            let before = if live.exists() {
                let identity = FileIdentity::read(&live)?;
                if declared.map(|f| &f.identity) != Some(&identity) {
                    return Err(failure("原有文件未在可信旧清单内或已被改变，拒绝覆盖"));
                }
                Some(identity)
            } else {
                if declared.is_some() {
                    return Err(failure("旧版文件缺失，需先修复"));
                }
                None
            };
            let permissions = if before.is_some() {
                Some(platform::capture_permissions(&live)?)
            } else {
                None
            };
            files.push(FileSnapshot {
                path: path.clone(),
                before,
                after: plan
                    .target_files
                    .iter()
                    .find(|f| f.path == path)
                    .map(|f| f.identity.clone()),
                permissions,
            });
        }
        let integration = platform::IntegrationSnapshot::capture(&plan.installation)?;
        fs::create_dir(&directory)?;
        storage::atomic_bytes(&directory.join("PRODUCT-OWNER.txt"), PRODUCT_ID.as_bytes())?;
        let transaction = directory
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| failure("事务名称无效"))?
            .to_owned();
        let mut this = Self {
            directory,
            journal: Journal {
                schema: JOURNAL_SCHEMA,
                product_id: PRODUCT_ID.into(),
                transaction,
                installation: plan.installation,
                previous_version: plan.previous_version,
                target_version: plan.target_version,
                release_id: plan.release_id,
                package_identity: plan.package_identity,
                phase: Phase::Preparing,
                files,
                integration,
                exit_code: None,
                failure: None,
                health_attempted: false,
                reboot_safety: RebootSafety::Unknown,
            },
        };
        this.persist()?;
        fault.checkpoint(Checkpoint::JournalCreated)?;
        for (i, file) in this.journal.files.iter().enumerate() {
            if let Some(before) = &file.before {
                let live = storage::owned_join(&this.journal.installation, &file.path)?;
                let backup = storage::owned_join(&this.directory.join("files"), &file.path)?;
                copy_verified(&live, &backup, before, false)?;
            }
            fault.checkpoint(Checkpoint::FileBackedUp(i))?;
        }
        this.journal.phase = Phase::Prepared;
        this.persist()?;
        fault.checkpoint(Checkpoint::Prepared)?;
        Ok(this)
    }
    pub fn open(directory: PathBuf) -> Result<Self> {
        storage::plain_path(&directory)?;
        if fs::read_to_string(directory.join("PRODUCT-OWNER.txt"))? != PRODUCT_ID {
            return Err(failure("恢复目录不属于本产品"));
        }
        let journal: Journal = storage::read_json(&directory.join("journal.json"))?;
        let this = Self { directory, journal };
        this.validate()?;
        Ok(this)
    }
    fn validate(&self) -> Result<()> {
        let j = &self.journal;
        if j.schema != JOURNAL_SCHEMA
            || j.product_id != PRODUCT_ID
            || j.files.len() > MAX_FILES
            || self.directory.starts_with(&j.installation)
            || self.directory.file_name().and_then(|p| p.to_str()) != Some(j.transaction.as_str())
        {
            return Err(failure("不支持或不一致的恢复事务"));
        }
        storage::plain_path(&j.installation)?;
        j.package_identity.validate()?;
        let mut paths = BTreeSet::new();
        for f in &j.files {
            storage::relative_path(&f.path)?;
            if !owned_payload_path(&f.path) || !paths.insert(f.path.to_lowercase()) {
                return Err(failure("恢复文件路径无效或重复"));
            }
            for id in [&f.before, &f.after].into_iter().flatten() {
                id.validate()?;
            }
        }
        j.integration.validate(&j.installation)?;
        Ok(())
    }
    fn persist(&self) -> Result<()> {
        self.validate()?;
        storage::atomic_json(&self.directory.join("journal.json"), &self.journal)
    }
    pub fn begin_install(&mut self, fault: &mut impl Faults) -> Result<()> {
        if self.journal.phase != Phase::Prepared {
            return Err(failure("安装事务未准备完成"));
        }
        self.verify_snapshot()?;
        self.journal.phase = Phase::Installing;
        self.persist()?;
        fault.checkpoint(Checkpoint::Installing)
    }
    pub fn installer_returned(
        &mut self,
        code: i32,
        reboot_queue_clear: bool,
        fault: &mut impl Faults,
    ) -> Result<()> {
        if self.journal.phase != Phase::Installing {
            return Err(failure("安装器返回状态不匹配"));
        }
        self.journal.exit_code = Some(code);
        self.journal.reboot_safety = if reboot_queue_clear && code != 3010 {
            RebootSafety::Clear
        } else {
            RebootSafety::Pending
        };
        self.persist()?;
        fault.checkpoint(Checkpoint::InstallerReturned)?;
        if !reboot_queue_clear || code == 3010 {
            self.journal.phase = Phase::RepairRequired;
            self.journal.failure =
                Some("安装涉及待重启操作；保留快照，需人工确认，不自动重启或覆盖恢复".into());
            self.persist()?;
            return Err(UpdateError::Repair(self.journal.failure.clone().unwrap()));
        }
        if code != 0 {
            return self.rollback("安装器失败", fault);
        }
        if let Err(e) = self.verify_target() {
            self.rollback("目标校验失败", fault)?;
            return Err(e);
        }
        self.journal.phase = Phase::PendingHealth;
        self.persist()?;
        fault.checkpoint(Checkpoint::PendingHealth)
    }
    fn verify_snapshot(&self) -> Result<()> {
        for f in &self.journal.files {
            if let Some(id) = &f.before {
                let p = storage::owned_join(&self.directory.join("files"), &f.path)?;
                if !id.matches(&p)? {
                    return Err(failure("快照哈希不符"));
                }
            }
        }
        Ok(())
    }
    pub fn verify_target(&self) -> Result<()> {
        for f in &self.journal.files {
            let p = storage::owned_join(&self.journal.installation, &f.path)?;
            if let Some(id) = &f.after {
                if !id.matches(&p)? {
                    return Err(failure("新版文件校验失败"));
                }
            } else if p.exists() {
                return Err(failure("旧版遗留文件尚未移除"));
            }
        }
        Ok(())
    }
    pub fn mark_health_attempt(&mut self) -> Result<()> {
        if self.journal.phase != Phase::PendingHealth || self.journal.health_attempted {
            return Err(UpdateError::Repair(
                "健康启动未完成，不重复运行失败版本".into(),
            ));
        }
        self.verify_target()?;
        self.journal.health_attempted = true;
        self.persist()
    }
    pub fn confirm_health(
        &mut self,
        health: &HealthReport,
        installed_state: &Path,
        fault: &mut impl Faults,
    ) -> Result<()> {
        if self.journal.phase != Phase::PendingHealth
            || !self.journal.health_attempted
            || health.version != self.journal.target_version
            || health.package_sha256 != self.journal.package_identity.sha256
            || health.updater_protocol != super::UPDATER_PROTOCOL
            || health.launcher_protocol != super::LAUNCHER_PROTOCOL
            || health.worker_protocol != crate::ai::model::WORKER_PROTOCOL
            || !health.settings_readable
            || !health.built_in_decode
            || !health.ui_event_loop
        {
            return Err(failure("健康握手内容不匹配或不完整"));
        }
        self.verify_target()?;
        fault.checkpoint(Checkpoint::BeforeHealthCommit)?;
        if installed_state != self.installed_state_path() {
            return Err(failure("已安装状态路径与事务不匹配"));
        }
        // Persist the health-commit intent first. Recovery can idempotently finish
        // the state projection; it must never roll healthy files back after this.
        self.journal.phase = Phase::CommittingHealth;
        self.persist()?;
        self.finish_health_commit()?;
        fault.checkpoint(Checkpoint::HealthCommitted)
    }
    fn installed_state_path(&self) -> PathBuf {
        self.directory.parent().unwrap().join("installed.state")
    }
    fn finish_health_commit(&mut self) -> Result<()> {
        if self.journal.phase != Phase::CommittingHealth {
            return Err(failure("健康提交状态无效"));
        }
        self.verify_target()?;
        let installed_state = self.installed_state_path();
        let previous: Option<InstalledState> = match storage::read_json(&installed_state) {
            Ok(s) => Some(s),
            Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if previous.as_ref().is_some_and(|p| {
            p.product_id != PRODUCT_ID || p.schema != 1 || p.release_id > self.journal.release_id
        }) {
            return Err(failure("已安装状态无效或发布序号倒退"));
        }
        let state = InstalledState {
            schema: 1,
            product_id: PRODUCT_ID.into(),
            version: self.journal.target_version.clone(),
            previous_healthy_version: self.journal.previous_version.clone(),
            release_id: self.journal.release_id,
            package_sha256: self.journal.package_identity.sha256.clone(),
            failed_releases: previous.map(|p| p.failed_releases).unwrap_or_default(),
        };
        storage::atomic_json(&installed_state, &state)?;
        self.journal.phase = Phase::Committed;
        self.persist()
    }
    pub fn recover_interrupted(&mut self, fault: &mut impl Faults) -> Result<()> {
        match self.journal.phase {
            Phase::CommittingHealth => self.finish_health_commit(),
            Phase::Preparing => {
                self.journal.phase = Phase::RolledBack;
                self.journal.failure = Some("准备阶段中断，应用文件尚未更改".into());
                self.persist()
            }
            Phase::Prepared => Ok(()),
            Phase::Installing => {
                if self.journal.reboot_safety == RebootSafety::Pending
                    || !platform::reboot_operations_clear()?
                {
                    self.journal.phase = Phase::RepairRequired;
                    self.journal.failure =
                        Some("中断安装存在或无法排除待重启操作，保留快照".into());
                    self.persist()?;
                    return Err(UpdateError::Repair(self.journal.failure.clone().unwrap()));
                }
                self.journal.reboot_safety = RebootSafety::Clear;
                self.persist()?;
                self.rollback("安装中断", fault)
            }
            Phase::RollingBack => self.rollback("恢复中断", fault),
            Phase::PendingHealth if self.journal.health_attempted => {
                self.rollback("上次新版健康握手未完成", fault)
            }
            Phase::RepairRequired => Err(UpdateError::Repair(
                self.journal.failure.clone().unwrap_or_default(),
            )),
            _ => Ok(()),
        }
    }
    pub fn rollback(&mut self, reason: &str, fault: &mut impl Faults) -> Result<()> {
        if matches!(self.journal.phase, Phase::Committed | Phase::RepairRequired) {
            return Err(UpdateError::Repair("此事务不能自动回退".into()));
        }
        self.verify_snapshot()?;
        self.journal.phase = Phase::RollingBack;
        self.journal.failure = Some(reason.chars().take(512).collect());
        self.persist()?;
        for (i, f) in self.journal.files.iter().enumerate() {
            let live = storage::owned_join(&self.journal.installation, &f.path)?;
            if live.exists() {
                let current = FileIdentity::read(&live)?;
                if Some(&current) != f.before.as_ref() && Some(&current) != f.after.as_ref() {
                    return Err(UpdateError::Repair(
                        "产品文件已被其他进程修改，保留现场与备份".into(),
                    ));
                }
            }
            if let Some(id) = &f.before {
                copy_verified(
                    &storage::owned_join(&self.directory.join("files"), &f.path)?,
                    &live,
                    id,
                    true,
                )?;
                if let Some(permissions) = &f.permissions {
                    platform::restore_permissions(&live, permissions)?;
                }
            } else if live.exists() {
                fs::remove_file(&live)?;
                storage::sync_directory(live.parent().unwrap())?;
            }
            fault.checkpoint(Checkpoint::FileRestored(i))?;
        }
        self.journal
            .integration
            .restore(&self.journal.installation)?;
        let quarantine = self
            .directory
            .parent()
            .unwrap()
            .join("failed-releases.json");
        let mut failed: Vec<u64> = match storage::read_json(&quarantine) {
            Ok(v) => v,
            Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        if !failed.contains(&self.journal.release_id) {
            failed.push(self.journal.release_id);
        }
        if failed.len() > 1024 {
            return Err(failure("失败发行隔离记录过大，需修复"));
        }
        storage::atomic_json(&quarantine, &failed)?;
        self.journal.phase = Phase::RolledBack;
        self.persist()
    }
    /// Cleanup is deliberately exact and preserves unknown files. A failure
    /// retains the complete journal so an operator can inspect it.
    pub fn cleanup_snapshot(&self) -> Result<()> {
        if !matches!(self.journal.phase, Phase::Committed | Phase::RolledBack) {
            return Err(failure("健康确认或完整回退前不能清理快照"));
        }
        for f in &self.journal.files {
            if let Some(id) = &f.before {
                let p = storage::owned_join(&self.directory.join("files"), &f.path)?;
                if p.exists() {
                    if !id.matches(&p)? {
                        return Err(failure("快照已变更，不清理"));
                    }
                    fs::remove_file(p)?;
                }
            }
        }
        // Keep journal + marker and any foreign files; never recursively delete.
        Ok(())
    }
}
fn copy_verified(
    source: &Path,
    destination: &Path,
    identity: &FileIdentity,
    replace: bool,
) -> Result<()> {
    storage::plain_path(source)?;
    storage::plain_path(destination)?;
    if !identity.matches(source)? {
        return Err(failure("复制源哈希不符"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| failure("目标目录无效"))?;
    fs::create_dir_all(parent)?;
    storage::plain_path(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, &mut temp)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    if !identity.matches(temp.path())? {
        return Err(failure("复制后哈希不符"));
    }
    storage::plain_path(destination)?;
    if replace {
        storage::persist_replace(temp, destination)?;
    } else {
        temp.persist_noclobber(destination).map_err(|e| e.error)?;
    }
    storage::sync_directory(parent)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        txn: PathBuf,
        plan: Plan,
    }
    impl Fixture {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().join("中文 app");
            fs::create_dir_all(root.join("runtime")).unwrap();
            fs::write(root.join("LIGHT-IMAGE-INSTALL.txt"), PRODUCT_ID).unwrap();
            fs::write(root.join("runtime/gloamere-image-ui.exe"), b"old ui").unwrap();
            fs::write(root.join("foreign.txt"), b"user own data").unwrap();
            let old = PayloadFile {
                path: "runtime/gloamere-image-ui.exe".into(),
                identity: FileIdentity::read(&root.join("runtime/gloamere-image-ui.exe")).unwrap(),
            };
            let n = d.path().join("new");
            fs::write(&n, b"new ui").unwrap();
            let new = PayloadFile {
                path: old.path.clone(),
                identity: FileIdentity::read(&n).unwrap(),
            };
            let txn = d.path().join("transaction-71");
            let plan = Plan {
                installation: root.clone(),
                previous_version: "0.7.0".into(),
                target_version: "0.7.1".into(),
                release_id: 71,
                package_identity: FileIdentity::read(&n).unwrap(),
                installed_files: vec![old],
                target_files: vec![new],
            };
            Self {
                _dir: d,
                root,
                txn,
                plan,
            }
        }
        fn install(&self) {
            fs::write(self.root.join("runtime/gloamere-image-ui.exe"), b"new ui").unwrap();
        }
        fn old_and_foreign(&self) {
            assert_eq!(
                fs::read(self.root.join("runtime/gloamere-image-ui.exe")).unwrap(),
                b"old ui"
            );
            assert_eq!(
                fs::read(self.root.join("foreign.txt")).unwrap(),
                b"user own data"
            );
        }
    }
    struct Stop(Checkpoint);
    impl Faults for Stop {
        fn checkpoint(&mut self, c: Checkpoint) -> Result<()> {
            if c == self.0 {
                Err(UpdateError::Io(std::io::Error::other(
                    "injected power loss",
                )))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn interruption_during_every_backup_and_commit_stage_restores_old() {
        for stop in [
            Checkpoint::JournalCreated,
            Checkpoint::FileBackedUp(0),
            Checkpoint::Prepared,
            Checkpoint::Installing,
            Checkpoint::InstallerReturned,
            Checkpoint::PendingHealth,
        ] {
            let f = Fixture::new();
            let mut fault = Stop(stop);
            let result = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut fault);
            if let Ok(mut txn) = result
                && txn.begin_install(&mut fault).is_ok()
            {
                f.install();
                let _ = txn.installer_returned(0, true, &mut fault);
                if txn.journal.phase == Phase::PendingHealth {
                    txn.mark_health_attempt().unwrap();
                }
            }
            let mut reopened = Transaction::open(f.txn.clone()).unwrap();
            reopened.recover_interrupted(&mut NoFault).unwrap();
            f.old_and_foreign();
        }
    }
    #[test]
    fn exact_health_commit_required_and_snapshot_retained_until_then() {
        let f = Fixture::new();
        let mut t = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut NoFault).unwrap();
        t.begin_install(&mut NoFault).unwrap();
        f.install();
        t.installer_returned(0, true, &mut NoFault).unwrap();
        assert!(t.cleanup_snapshot().is_err());
        t.mark_health_attempt().unwrap();
        let mut h = HealthReport {
            version: "0.7.1".into(),
            package_sha256: f.plan.package_identity.sha256.clone(),
            updater_protocol: 1,
            launcher_protocol: 1,
            worker_protocol: crate::ai::model::WORKER_PROTOCOL,
            settings_readable: true,
            built_in_decode: true,
            ui_event_loop: false,
        };
        let state = f._dir.path().join("installed.state");
        assert!(t.confirm_health(&h, &state, &mut NoFault).is_err());
        h.ui_event_loop = true;
        t.confirm_health(&h, &state, &mut NoFault).unwrap();
        assert_eq!(t.journal.phase, Phase::Committed);
        assert_eq!(
            storage::read_json::<InstalledState>(&state)
                .unwrap()
                .version,
            "0.7.1"
        );
        assert!(f.txn.join("files/runtime/gloamere-image-ui.exe").exists());
        t.cleanup_snapshot().unwrap();
        assert!(f.root.join("foreign.txt").exists());
    }
    #[test]
    fn unknown_exit_reboot_and_modified_files_do_not_claim_success() {
        for (code, reboot) in [(3010, true), (0, false)] {
            let f = Fixture::new();
            let mut t = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut NoFault).unwrap();
            t.begin_install(&mut NoFault).unwrap();
            f.install();
            assert!(t.installer_returned(code, reboot, &mut NoFault).is_err());
            assert_eq!(t.journal.phase, Phase::RepairRequired);
            assert!(t.cleanup_snapshot().is_err());
        }
        let f = Fixture::new();
        let mut t = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut NoFault).unwrap();
        t.begin_install(&mut NoFault).unwrap();
        f.install();
        t.installer_returned(99, true, &mut NoFault).unwrap();
        assert_eq!(t.journal.phase, Phase::RolledBack);
        f.old_and_foreign();
    }
    #[test]
    fn foreign_replacement_and_snapshot_corruption_are_retained() {
        let f = Fixture::new();
        let mut t = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut NoFault).unwrap();
        t.begin_install(&mut NoFault).unwrap();
        fs::write(
            f.root.join("runtime/gloamere-image-ui.exe"),
            b"foreign replacement",
        )
        .unwrap();
        assert!(t.rollback("failed", &mut NoFault).is_err());
        assert_eq!(
            fs::read(f.root.join("runtime/gloamere-image-ui.exe")).unwrap(),
            b"foreign replacement"
        );
        assert!(f.txn.join("files/runtime/gloamere-image-ui.exe").exists());
    }
    #[test]
    fn rollback_is_idempotent_across_interruption() {
        let f = Fixture::new();
        let mut t = Transaction::prepare(f.txn.clone(), f.plan.clone(), &mut NoFault).unwrap();
        t.begin_install(&mut NoFault).unwrap();
        f.install();
        assert!(
            t.rollback("failure", &mut Stop(Checkpoint::FileRestored(0)))
                .is_err()
        );
        let mut t = Transaction::open(f.txn.clone()).unwrap();
        t.recover_interrupted(&mut NoFault).unwrap();
        assert_eq!(t.journal.phase, Phase::RolledBack);
        f.old_and_foreign();
    }
    #[test]
    fn stable_launcher_user_settings_and_escape_paths_never_enter_payload() {
        for path in [
            "gloamere-image.exe",
            "../settings.json",
            "settings.json",
            "ai-records.sqlite",
            "runtime/../x",
        ] {
            assert!(
                validate_payload(&[PayloadFile {
                    path: path.into(),
                    identity: FileIdentity {
                        length: 1,
                        sha256: "a".repeat(64)
                    }
                }])
                .is_err()
            );
        }
    }
}
