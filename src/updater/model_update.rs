//! Independent installed-model updates. Signed metadata chooses a ZIP, never an
//! arbitrary graph or DLL. Activation is a single owned pointer replacement.
use super::{Result, UpdateError, client::Preferences, engine, storage};
use crate::ai::model;
use safeupdate_core::{
    CancellationToken, ModelCompatibility, ProductionConfig, VerifiedModelTarget,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

pub const MODEL_ID: &str = "code-linear-fp32-experimental";
pub const LICENSE_POLICY: &str = "gloamere-code-ort1283-ljt3141-license-set-v1";
const MAX_ZIP_BYTES: u64 = 128 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const POINTER_FILE: &str = "active-model.json";
const LEGACY_DIRECTORY: &str = "code-linear-fp32";

fn security(error: impl std::fmt::Display) -> UpdateError {
    UpdateError::Security(error.to_string())
}
fn hex(hash: &[u8]) -> String {
    hash.iter().map(|v| format!("{v:02x}")).collect()
}
fn cancelled(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(UpdateError::Deferred("模型更新已取消".into()))
    } else {
        Ok(())
    }
}

fn ensure_private_directory(root: &Path) -> Result<()> {
    storage::plain_path(root)?;
    let _pins = super::platform::ensure_private_data_directory(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    super::platform::verify_current_user_path(root)
}

/// Content identity, not model-quality approval. No path is read from remote metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledModel {
    model_id: String,
    release_id: u64,
    package_sha256: Option<String>,
    profile: String,
}
impl InstalledModel {
    fn legacy() -> Self {
        Self {
            model_id: MODEL_ID.into(),
            release_id: 0,
            package_sha256: None,
            profile: model::fingerprint(),
        }
    }
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
    pub fn release_id(&self) -> u64 {
        self.release_id
    }
    pub fn revision(&self) -> Option<String> {
        self.package_sha256
            .as_ref()
            .map(|hash| format!("{}:{hash}", self.release_id))
    }
    pub fn directory(&self, root: &Path) -> Result<PathBuf> {
        self.validate()?;
        let relative = match &self.package_sha256 {
            Some(hash) => format!("versions/{}-{hash}", self.release_id),
            None => LEGACY_DIRECTORY.into(),
        };
        storage::owned_join(root, &relative)
    }
    fn validate(&self) -> Result<()> {
        if self.model_id != MODEL_ID
            || self.profile != model::fingerprint()
            || match &self.package_sha256 {
                Some(hash) => {
                    self.release_id == 0
                        || !crate::ai::types::is_content_hash(hash)
                        || hash.bytes().any(|v| v.is_ascii_uppercase())
                }
                None => self.release_id != 0,
            }
        {
            return Err(security("模型指针不属于当前编译的兼容配置"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivePointer {
    schema: u32,
    current: Option<InstalledModel>,
    previous: Option<InstalledModel>,
}
fn pointer(root: &Path) -> Result<Option<ActivePointer>> {
    match storage::read_json::<ActivePointer>(&root.join(POINTER_FILE)) {
        Ok(p) => {
            if p.schema != 1 {
                return Err(security("模型指针版本未知，保留原文件"));
            }
            if let Some(model) = &p.current {
                model.validate()?;
            }
            if let Some(model) = &p.previous {
                model.validate()?;
            }
            Ok(Some(p))
        }
        Err(UpdateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
pub fn installed(root: &Path) -> Result<Option<InstalledModel>> {
    storage::plain_path(root)?;
    if let Some(pointer) = pointer(root)? {
        return Ok(pointer.current);
    }
    let legacy = InstalledModel::legacy();
    Ok(legacy
        .directory(root)?
        .join("manifest.json")
        .is_file()
        .then_some(legacy))
}

fn manifest_pin() -> Result<(&'static str, u64)> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok((
            "900966df9db55dd65ac2431aed44c37166c0605cfd136d1bca6198bc965180ab",
            39_634_074,
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok((
            "e7c1e7b365eb87253000543dcd0e1f56fdf090e026925381791fc80da2d47b1d",
            47_858_688,
        ))
    } else {
        Err(security("此平台没有已编译核验的模型发行配置"))
    }
}
pub fn compatibility(
    installed: &InstalledModel,
    installation_id: String,
) -> Result<ModelCompatibility> {
    installed.validate()?;
    let (manifest_sha256, unpacked_length) = manifest_pin()?;
    Ok(ModelCompatibility {
        installed_release_id: installed.release_id,
        app_protocol: 1,
        installation_id,
        model_id: MODEL_ID.into(),
        model_version: model::MODEL_HASH.into(),
        pack_schema: 2,
        preprocess_version: model::PREPROCESS.into(),
        worker_protocol: model::WORKER_PROTOCOL,
        runtime_version: "1.28.3".into(),
        source_revision: "c2a0c9216cd8b96e91927848cb461ecbdfc5a329".into(),
        pack_fingerprint: model::fingerprint(),
        license_policy: LICENSE_POLICY.into(),
        license_files: model::license_files().iter().map(|s| (*s).into()).collect(),
        manifest_sha256: manifest_sha256.into(),
        unpacked_length,
    })
}

/// Fixed flat inventory and per-entry ceilings, independent of ZIP metadata.
fn inventory() -> BTreeMap<String, u64> {
    let mut items = model::trusted_component_files()
        .into_iter()
        .map(|f| (f.name, f.bytes))
        .collect::<BTreeMap<_, _>>();
    items.insert("manifest.json".into(), 16 * 1024);
    for name in model::license_files() {
        items.insert((*name).into(), 1024 * 1024);
    }
    items
}
/// Reject ZIP64/SFX/multi-volume and oversized central directories before the
/// general ZIP parser can allocate from attacker-controlled entry counts.
fn bounded_zip_header(file: &mut File, entries: usize) -> Result<()> {
    let length = file.metadata()?.len();
    if !(22..=MAX_ZIP_BYTES).contains(&length) {
        return Err(security("模型 ZIP 大小超限"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if magic != *b"PK\x03\x04" {
        return Err(security("模型 ZIP 不接受自解压或前置数据"));
    }
    file.seek(SeekFrom::End(-22))?;
    let mut end = [0u8; 22];
    file.read_exact(&mut end)?;
    let u16_at = |p| u16::from_le_bytes([end[p], end[p + 1]]);
    let u32_at = |p| u32::from_le_bytes([end[p], end[p + 1], end[p + 2], end[p + 3]]);
    if end[..4] != *b"PK\x05\x06"
        || u16_at(4) != 0
        || u16_at(6) != 0
        || usize::from(u16_at(8)) != entries
        || usize::from(u16_at(10)) != entries
        || u16_at(20) != 0
        || u32_at(12) > 32 * 1024
        || u64::from(u32_at(16)) + u64::from(u32_at(12)) != length - 22
    {
        return Err(security("模型 ZIP 目录、条目数或多卷格式不符合固定包规范"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}
fn unpack_zip(
    zip_path: &Path,
    stage: &Path,
    expected_length: u64,
    expected_hash: &str,
    expected_expanded: u64,
    cancel: &CancellationToken,
) -> Result<()> {
    storage::plain_path(zip_path)?;
    storage::plain_path(stage)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
    }
    let mut file = options.open(zip_path)?;
    let (length, hash) = storage::hash_reader(&mut file)?;
    if length != expected_length || hash != expected_hash || expected_expanded > MAX_EXPANDED_BYTES
    {
        return Err(security("模型 ZIP 与签名目标身份不符"));
    }
    let inventory = inventory();
    bounded_zip_header(&mut file, inventory.len())?;
    let mut zip = zip::ZipArchive::new(file).map_err(security)?;
    if zip.len() != inventory.len() {
        return Err(security("模型 ZIP 文件清单不匹配"));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        cancelled(cancel)?;
        let mut entry = zip.by_index(index).map_err(security)?;
        let name = entry.name().to_string();
        let max = inventory
            .get(&name)
            .ok_or_else(|| security("模型 ZIP 包含未批准文件"))?;
        if entry.name_raw() != name.as_bytes()
            || !seen.insert(name.clone())
            || entry.is_dir()
            || entry.is_symlink()
            || !entry.is_file()
            || entry.encrypted()
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
            || entry.size() == 0
            || entry.size() > *max
            || entry.compressed_size() > MAX_ZIP_BYTES
        {
            return Err(security("模型 ZIP 链接、重复名称、加密或文件大小无效"));
        }
        let target = storage::owned_join(stage, &name)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let mut copied = 0u64;
        let mut bytes = [0u8; 65536];
        loop {
            cancelled(cancel)?;
            let count = entry.read(&mut bytes).map_err(security)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| security("模型解压计数溢出"))?;
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| security("模型解压计数溢出"))?;
            if copied > *max || total > expected_expanded {
                return Err(security("模型解压超过签名清单预算"));
            }
            output.write_all(&bytes[..count])?;
        }
        if copied != entry.size() {
            return Err(security("模型 ZIP 实际长度不匹配"));
        }
        output.sync_all()?;
    }
    if total != expected_expanded {
        return Err(security("模型解压总长度与签名目标不符"));
    }
    let mut file = zip.into_inner();
    file.seek(SeekFrom::Start(0))?;
    if storage::hash_reader(&mut file)? != (expected_length, expected_hash.to_string()) {
        return Err(security("模型 ZIP 在读取期间发生变化"));
    }
    storage::sync_directory(stage)
}
fn validate_staged(path: &Path) -> Result<model::VerifiedPack> {
    let inventory = inventory();
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| security("模型目录名称无效"))?;
        if !inventory.contains_key(&name) || !entry.file_type()?.is_file() {
            return Err(security("模型暂存目录包含未知文件或链接"));
        }
        actual.insert(name);
    }
    if actual.len() != inventory.len() {
        return Err(security("模型暂存清单不完整"));
    }
    let (manifest_hash, expanded) = manifest_pin()?;
    if storage::hash_file(&path.join("manifest.json"))?.1 != manifest_hash {
        return Err(security("模型清单不在编译白名单"));
    }
    #[derive(Deserialize)]
    struct PinnedFile {
        bytes: u64,
        sha256: String,
    }
    let platforms: BTreeMap<String, BTreeMap<String, PinnedFile>> =
        serde_json::from_str(include_str!("../ai/approved-model-update-inventory.json"))?;
    let platform = if cfg!(windows) {
        "win-x64"
    } else {
        "linux-x64"
    };
    let approved = platforms
        .get(platform)
        .ok_or_else(|| security("没有此平台的固定许可内容清单"))?;
    if approved.len() != inventory.len() {
        return Err(security("编译组件清单不完整"));
    }
    for (name, expected) in approved {
        if !inventory.contains_key(name)
            || storage::hash_file(&path.join(name))? != (expected.bytes, expected.sha256.clone())
        {
            return Err(security("模型组件或许可文本不在固定内容白名单"));
        }
    }
    let pack = model::validate_pack(path).map_err(security)?;
    if model::component_info(&pack)
        .map_err(security)?
        .installed_bytes
        != expanded
    {
        return Err(security("模型实际展开大小不匹配"));
    }
    Ok(pack)
}

/// An owned preparation result, not serializable or constructible from disk JSON.
pub struct PreparedModelUpdate {
    update_root: PathBuf,
    model_root: PathBuf,
    config: ProductionConfig,
    target: VerifiedModelTarget,
    previous: InstalledModel,
    next: InstalledModel,
    compatibility: ModelCompatibility,
    cancel: CancellationToken,
    info: model::ComponentInfo,
    publication: Arc<Mutex<bool>>,
}
impl PreparedModelUpdate {
    pub fn version(&self) -> &str {
        &self.target.release().model_version
    }
    pub fn model_root(&self) -> &Path {
        &self.model_root
    }
    /// Called on the UI thread with its current independent model-update choice.
    /// No asynchronous step intervenes after the persisted preference check.
    pub fn activate(
        self,
        currently_allowed: bool,
    ) -> Result<(PathBuf, model::ComponentInfo, InstalledModel)> {
        cancelled(&self.cancel)?;
        if !currently_allowed {
            return Err(UpdateError::Deferred("已关闭模型组件自动更新".into()));
        }
        let _pins = super::platform::pin_directories(&self.model_root)?;
        let _lock = storage::LockGuard::acquire(&self.model_root.join("activation.lock"), false)?;
        if installed(&self.model_root)?.as_ref() != Some(&self.previous) {
            return Err(UpdateError::Deferred(
                "当前模型已经变化，丢弃过期准备结果".into(),
            ));
        }
        let directory = self.next.directory(&self.model_root)?;
        if let Err(error) = validate_staged(&directory) {
            quarantine_failed(&self.model_root, self.next.release_id)?;
            return Err(error);
        }
        let mut state = engine::open_trust(&self.update_root, &self.config)?;
        safeupdate_core::revalidate_model_for_activation(
            &self.config,
            &self.target,
            &mut state,
            &self.compatibility,
        )
        .map_err(security)?;
        publish_activation(
            &self.model_root,
            &self.update_root,
            &self.previous,
            &self.next,
            currently_allowed,
            &self.cancel,
            &self.publication,
        )?;
        Ok((directory, self.info, self.next))
    }
}

fn publish_activation(
    root: &Path,
    update_root: &Path,
    previous: &InstalledModel,
    next: &InstalledModel,
    currently_allowed: bool,
    cancel: &CancellationToken,
    publication: &Mutex<bool>,
) -> Result<()> {
    let gate = publication
        .lock()
        .map_err(|_| security("模型提交门禁异常"))?;
    if *gate || !currently_allowed {
        return Err(UpdateError::Deferred("模型更新已取消或关闭".into()));
    }
    let settings = update_root
        .parent()
        .ok_or_else(|| security("更新设置路径无效"))?
        .join("update-settings.json");
    let preferences = Preferences::load(&settings)?;
    cancelled(cancel)?;
    if !preferences.automatic_installed_model_updates {
        return Err(UpdateError::Deferred("模型组件自动更新授权已撤回".into()));
    }
    if installed(root)?.as_ref() != Some(previous) {
        return Err(UpdateError::Deferred("当前模型变化，取消过期更新".into()));
    }
    storage::atomic_json(
        &root.join(POINTER_FILE),
        &ActivePointer {
            schema: 1,
            current: Some(next.clone()),
            previous: Some(previous.clone()),
        },
    )
}

pub struct ModelJob {
    pub receiver: mpsc::Receiver<Result<Option<PreparedModelUpdate>>>,
    cancel: CancellationToken,
    publication: Arc<Mutex<bool>>,
    thread: Option<JoinHandle<()>>,
    keep_partial: Arc<AtomicBool>,
}
impl ModelJob {
    pub fn start(
        update_root: PathBuf,
        model_root: PathBuf,
        config: ProductionConfig,
        previous: InstalledModel,
        allow_metered_once: bool,
        heavy_work: bool,
    ) -> Self {
        Self::spawn(move |cancel, gate, keep_partial| {
            prepare(
                update_root,
                model_root,
                config,
                previous,
                allow_metered_once,
                heavy_work,
                cancel,
                gate,
                keep_partial,
            )
        })
    }
    /// Reuses staging after a trusted downloader supplies an opaque verified
    /// target. Neither a path nor caller-written JSON can manufacture authority.
    pub fn stage_verified_archive(
        update_root: PathBuf,
        model_root: PathBuf,
        config: ProductionConfig,
        previous: InstalledModel,
        target: VerifiedModelTarget,
        archive: PathBuf,
    ) -> Self {
        Self::spawn(move |cancel, gate, _keep_partial| {
            stage_verified(
                update_root,
                model_root,
                config,
                previous,
                target,
                archive,
                cancel,
                gate,
            )
            .map(Some)
        })
    }
    fn spawn(
        operation: impl FnOnce(
            CancellationToken,
            Arc<Mutex<bool>>,
            Arc<AtomicBool>,
        ) -> Result<Option<PreparedModelUpdate>>
        + Send
        + 'static,
    ) -> Self {
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let publication = Arc::new(Mutex::new(false));
        let gate = publication.clone();
        let keep_partial = Arc::new(AtomicBool::new(false));
        let preserve = keep_partial.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                operation(worker_cancel, gate, preserve)
            }))
            .unwrap_or_else(|_| Err(security("模型更新后台任务异常，保留旧组件")));
            let _ = tx.send(result);
        });
        Self {
            receiver,
            cancel,
            publication,
            thread: Some(thread),
            keep_partial,
        }
    }
    pub fn pause(&self) {
        self.keep_partial.store(true, Ordering::Release);
        self.stop();
    }
    pub fn cancel(&self) {
        self.keep_partial.store(false, Ordering::Release);
        self.stop();
    }
    fn stop(&self) {
        if let Ok(mut gate) = self.publication.lock() {
            *gate = true;
            self.cancel.cancel();
        } else {
            self.cancel.cancel();
        }
    }
}
impl Drop for ModelJob {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn prepare(
    update_root: PathBuf,
    model_root: PathBuf,
    config: ProductionConfig,
    previous: InstalledModel,
    allow_metered_once: bool,
    heavy_work: bool,
    cancel: CancellationToken,
    publication: Arc<Mutex<bool>>,
    keep_partial: Arc<AtomicBool>,
) -> Result<Option<PreparedModelUpdate>> {
    ensure_private_directory(&model_root)?;
    let mut _pins = super::platform::pin_directories(&model_root)?;
    if installed(&model_root)?.as_ref() != Some(&previous) {
        return Err(security("自动模型更新只适用于已安装的同一模型"));
    }
    model::validate_pack(&previous.directory(&model_root)?).map_err(security)?;
    let mut state = engine::open_trust(&update_root, &config)?;
    for failed in failures(&model_root)? {
        state
            .quarantine_model_release(MODEL_ID, failed)
            .map_err(security)?;
    }
    let installation_id = engine::compatibility(&update_root)?.installation_id;
    let compatibility = compatibility(&previous, installation_id)?;
    let Some(target) =
        safeupdate_core::blocking_check_model(&config, &mut state, &compatibility, &cancel)
            .map_err(security)?
    else {
        return Ok(None);
    };
    cancelled(&cancel)?;
    let mut resources = super::platform::download_resources(&update_root, heavy_work)?;
    resources.install_free =
        super::platform::download_resources(&model_root, heavy_work)?.download_free;
    resources.download_permission(
        allow_metered_once,
        target.length(),
        target.release().unpacked_length,
        model::component_info(
            &model::validate_pack(&previous.directory(&model_root)?).map_err(security)?,
        )
        .map_err(security)?
        .installed_bytes,
    )?;
    let downloads = update_root.join("model-downloads");
    ensure_private_directory(&downloads)?;
    _pins.extend(super::platform::pin_directories(&downloads)?);
    let package_hash = hex(target.sha256());
    let archive = downloads.join(format!(
        "{}-{package_hash}.zip",
        target.release().release_id
    ));
    safeupdate_core::revalidate_model_for_activation(&config, &target, &mut state, &compatibility)
        .map_err(security)?;
    if archive.exists() {
        if storage::hash_file(&archive)? != (target.length(), package_hash.clone()) {
            return Err(security("已有模型下载与可信目标冲突，保留现场"));
        }
    } else {
        let download =
            safeupdate_core::blocking_download_model(&config, &target, &archive, &cancel);
        if cancel.is_cancelled() {
            if !keep_partial.load(Ordering::Acquire) {
                clear_cancelled_partial(&archive)?;
            }
            cancelled(&cancel)?;
        }
        download.map_err(security)?;
    }
    drop(state);
    stage_verified(
        update_root,
        model_root,
        config,
        previous,
        target,
        archive,
        cancel,
        publication,
    )
    .map(Some)
}
#[allow(clippy::too_many_arguments)]
fn stage_verified(
    update_root: PathBuf,
    model_root: PathBuf,
    config: ProductionConfig,
    previous: InstalledModel,
    target: VerifiedModelTarget,
    archive: PathBuf,
    cancel: CancellationToken,
    publication: Arc<Mutex<bool>>,
) -> Result<PreparedModelUpdate> {
    cancelled(&cancel)?;
    ensure_private_directory(&model_root)?;
    let mut _pins = super::platform::pin_directories(&model_root)?;
    _pins.extend(super::platform::pin_directories(
        archive.parent().ok_or_else(|| security("模型包路径无效"))?,
    )?);
    if installed(&model_root)?.as_ref() != Some(&previous) {
        return Err(security("模型更新不能安装新模型或替换已变化的组件"));
    }
    model::validate_pack(&previous.directory(&model_root)?).map_err(security)?;
    let mut state = engine::open_trust(&update_root, &config)?;
    for failed in failures(&model_root)? {
        state
            .quarantine_model_release(MODEL_ID, failed)
            .map_err(security)?;
    }
    let compatibility = compatibility(
        &previous,
        engine::compatibility(&update_root)?.installation_id,
    )?;
    safeupdate_core::revalidate_model_for_activation(&config, &target, &mut state, &compatibility)
        .map_err(security)?;
    drop(state);
    let package_hash = hex(target.sha256());
    let versions = model_root.join("versions");
    ensure_private_directory(&versions)?;
    _pins.extend(super::platform::pin_directories(&versions)?);
    ensure_model_volume_space(&model_root, target.release().unpacked_length)?;
    let stage = tempfile::Builder::new()
        .prefix("model-stage-")
        .tempdir_in(&versions)?;
    let verification = (|| {
        unpack_zip(
            &archive,
            stage.path(),
            target.length(),
            &package_hash,
            target.release().unpacked_length,
            &cancel,
        )?;
        validate_staged(stage.path())?;
        model::validate_and_self_test(stage.path()).map_err(security)
    })();
    let verified = match verification {
        Ok(pack) => pack,
        Err(error) => {
            if matches!(error, UpdateError::Security(_)) {
                quarantine_failed(&model_root, target.release().release_id)?;
            }
            return Err(error);
        }
    };
    let info = model::component_info(&verified).map_err(security)?;
    cancelled(&cancel)?;
    let next = InstalledModel {
        model_id: MODEL_ID.into(),
        release_id: target.release().release_id,
        package_sha256: Some(package_hash),
        profile: model::fingerprint(),
    };
    let _activation = storage::LockGuard::acquire(&model_root.join("activation.lock"), false)?;
    let directory = next.directory(&model_root)?;
    if directory.exists() {
        validate_staged(&directory)?;
        model::validate_and_self_test(&directory).map_err(security)?;
    } else {
        ensure_model_volume_space(&model_root, 0)?;
        promote_directory(stage.path(), &directory)?;
    }
    cancelled(&cancel)?;
    Ok(PreparedModelUpdate {
        update_root,
        model_root,
        config,
        target,
        previous,
        next,
        compatibility,
        cancel,
        info,
        publication,
    })
}

fn ensure_model_volume_space(root: &Path, new_bytes: u64) -> Result<()> {
    #[cfg(windows)]
    {
        let free = super::platform::download_resources(root, false)?.download_free;
        let needed = new_bytes
            .checked_add(256 * 1024 * 1024)
            .ok_or_else(|| security("模型空间预算溢出"))?;
        if free < needed {
            return Err(UpdateError::Deferred(
                "模型所在磁盘空间不足，保留旧组件".into(),
            ));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (root, new_bytes);
    } // Offline fixture adapter; production downloads already fail closed on this platform.
    Ok(())
}
/// Called only for this task's content-addressed destination, after the
/// downloader has returned and released its lock. Never recursive.
fn clear_cancelled_partial(archive: &Path) -> Result<()> {
    let _lock = storage::LockGuard::acquire(&archive.with_extension("zip.download.lock"), false)?;
    for suffix in ["zip.part", "zip.part.json"] {
        let path = archive.with_extension(suffix);
        storage::plain_path(&path)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => fs::remove_file(path)?,
            Ok(_) => return Err(security("模型下载暂存不是普通文件，保留现场")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn failures(root: &Path) -> Result<Vec<u64>> {
    let failures: Vec<u64> = match storage::read_json(&root.join("failed-model-releases.json")) {
        Ok(v) => v,
        Err(UpdateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if failures.len() > 1024 {
        return Err(security("模型失败隔离记录过大"));
    }
    Ok(failures)
}
fn quarantine_failed(root: &Path, release_id: u64) -> Result<()> {
    let _lock = storage::LockGuard::acquire(&root.join("failed-model-releases.lock"), false)?;
    let mut failed = failures(root)?;
    if release_id > 0 && !failed.contains(&release_id) {
        if failed.len() >= 1024 {
            return Err(security("模型失败隔离记录已满"));
        }
        failed.push(release_id);
        storage::atomic_json(&root.join("failed-model-releases.json"), &failed)?;
    }
    Ok(())
}
/// Startup self-test and exact retained-component recovery. Trust floors are not
/// reset, and the failed sequence is quarantined before the previous pointer wins.
pub fn validate_installed_or_recover(
    root: &Path,
) -> Result<Option<(PathBuf, model::ComponentInfo, InstalledModel)>> {
    let _pins = super::platform::pin_directories(root)?;
    let Some(current) = installed(root)? else {
        return Ok(None);
    };
    let current_path = current.directory(root)?;
    match validate_installed_component(&current, &current_path) {
        Ok(pack) => Ok(Some((
            current_path,
            model::component_info(&pack).map_err(security)?,
            current,
        ))),
        Err(error) => {
            let old = pointer(root)?
                .and_then(|p| p.previous)
                .ok_or_else(|| security(error))?;
            let path = old.directory(root)?;
            let pack = validate_installed_component(&old, &path)?;
            let _lock = storage::LockGuard::acquire(&root.join("activation.lock"), false)?;
            if installed(root)?.as_ref() != Some(&current) {
                return Err(UpdateError::Deferred(
                    "模型指针在恢复期间变化，请重试".into(),
                ));
            }
            quarantine_failed(root, current.release_id)?;
            storage::atomic_json(
                &root.join(POINTER_FILE),
                &ActivePointer {
                    schema: 1,
                    current: Some(old.clone()),
                    previous: None,
                },
            )?;
            Ok(Some((
                path,
                model::component_info(&pack).map_err(security)?,
                old,
            )))
        }
    }
}
/// Explicit deletion gets a tombstone so retained/legacy folders cannot silently
/// reactivate on restart. Unrecognized files are never deleted by this function.
pub fn unregister(root: &Path) -> Result<Vec<PathBuf>> {
    ensure_private_directory(root)?;
    let _pins = super::platform::pin_directories(root)?;
    let _lock = storage::LockGuard::acquire(&root.join("activation.lock"), false)?;
    let mut paths = Vec::new();
    if let Some(p) = pointer(root)? {
        for item in [p.current, p.previous].into_iter().flatten() {
            let path = item.directory(root)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let legacy = root.join(LEGACY_DIRECTORY);
    if legacy.join("manifest.json").is_file() && !paths.contains(&legacy) {
        paths.push(legacy);
    }
    storage::atomic_json(
        &root.join(POINTER_FILE),
        &ActivePointer {
            schema: 1,
            current: None,
            previous: None,
        },
    )?;
    Ok(paths)
}
/// Offline installation shares the activation lock with signed updates. It never
/// replaces an existing component and publishes only after the fixed self-test.
pub fn import_offline(
    source: &Path,
    root: &Path,
) -> Result<(PathBuf, model::ComponentInfo, InstalledModel)> {
    ensure_private_directory(root)?;
    let _pins = super::platform::pin_directories(root)?;
    let _lock = storage::LockGuard::acquire(&root.join("activation.lock"), false)?;
    if installed(root)?.is_some() {
        return Err(security("组件已安装，请先明确移除再离线导入"));
    }
    let descriptor = InstalledModel::legacy();
    let path = descriptor.directory(root)?;
    model::install_pack(source, &path).map_err(security)?;
    let pack = model::validate_pack(&path).map_err(security)?;
    let info = model::component_info(&pack).map_err(security)?;
    storage::atomic_json(
        &root.join(POINTER_FILE),
        &ActivePointer {
            schema: 1,
            current: Some(descriptor.clone()),
            previous: None,
        },
    )?;
    Ok((path, info, descriptor))
}
fn validate_installed_component(
    installed: &InstalledModel,
    path: &Path,
) -> Result<model::VerifiedPack> {
    if installed.release_id > 0 {
        validate_staged(path)?;
    }
    model::validate_and_self_test(path).map_err(security)
}
fn promote_directory(source: &Path, destination: &Path) -> Result<()> {
    storage::plain_path(source)?;
    storage::plain_path(destination)?;
    if destination.exists() {
        return Err(security("模型版本目录已存在，拒绝覆盖"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // No REPLACE_EXISTING: a concurrent valid version can never be overwritten.
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)?;
    }
    storage::sync_directory(
        destination
            .parent()
            .ok_or_else(|| security("模型版本父目录无效"))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::test_support::{self, fs};
    use zip::write::SimpleFileOptions;
    fn tiny_archive(
        path: &Path,
        rename: Option<(&str, &str)>,
        symlink: bool,
        comment: bool,
    ) -> (u64, String, u64) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let mut total = 0;
        for (index, name) in inventory().keys().enumerate() {
            let name = rename
                .filter(|(before, _)| *before == name)
                .map(|(_, after)| after)
                .unwrap_or(name);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            if symlink && index == 0 {
                writer.add_symlink(name, "x", options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(b"x").unwrap();
            }
            total += 1;
        }
        if comment {
            writer
                .set_comment("no comments in fixed pack format")
                .unwrap();
        }
        writer.finish().unwrap().sync_all().unwrap();
        let (length, hash) = storage::hash_file(path).unwrap();
        (length, hash, total)
    }
    fn unpack_fixture(
        path: &Path,
        stage: &Path,
        expanded: u64,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (length, hash) = storage::hash_file(path)?;
        unpack_zip(path, stage, length, &hash, expanded, cancel)
    }
    #[test]
    fn exact_flat_bounded_zip_extracts_but_untrusted_bytes_never_validate() {
        let dir = test_support::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        let (_, _, total) = tiny_archive(&path, None, false, false);
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        unpack_fixture(&path, stage.path(), total, &CancellationToken::new()).unwrap();
        assert_eq!(
            fs::read_dir(stage.path()).unwrap().count(),
            inventory().len()
        );
        assert!(validate_staged(stage.path()).is_err());
    }
    #[test]
    fn traversal_unknown_and_symlink_archives_are_rejected() {
        for replacement in [
            "../escape.bin",
            "/absolute.bin",
            "unknown.dll",
            "C:\\evil.dll",
        ] {
            let dir = test_support::tempdir().unwrap();
            let path = dir.path().join("pack.zip");
            let (_, _, total) =
                tiny_archive(&path, Some(("manifest.json", replacement)), false, false);
            let stage = tempfile::tempdir_in(dir.path()).unwrap();
            assert!(unpack_fixture(&path, stage.path(), total, &CancellationToken::new()).is_err());
            assert!(!dir.path().join("escape.bin").exists());
        }
        let dir = test_support::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        let (_, _, total) = tiny_archive(&path, None, true, false);
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        assert!(unpack_fixture(&path, stage.path(), total, &CancellationToken::new()).is_err());
    }
    #[test]
    fn encrypted_flags_duplicate_names_and_expansion_budgets_rejected() {
        let dir = test_support::tempdir().unwrap();
        let original = dir.path().join("pack.zip");
        let (_, _, total) = tiny_archive(&original, None, false, false);
        let baseline = fs::read(&original).unwrap();
        let encrypted = dir.path().join("encrypted.zip");
        let mut bytes = baseline.clone();
        for index in 0..bytes.len() - 10 {
            if bytes[index..index + 4] == *b"PK\x03\x04" {
                bytes[index + 6] |= 1;
            } else if bytes[index..index + 4] == *b"PK\x01\x02" {
                bytes[index + 8] |= 1;
            }
        }
        fs::write(&encrypted, bytes).unwrap();
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        assert!(
            unpack_fixture(&encrypted, stage.path(), total, &CancellationToken::new()).is_err()
        );
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        assert!(
            unpack_fixture(
                &original,
                stage.path(),
                total - 1,
                &CancellationToken::new()
            )
            .is_err()
        );
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        assert!(
            unpack_fixture(
                &original,
                stage.path(),
                MAX_EXPANDED_BYTES + 1,
                &CancellationToken::new()
            )
            .is_err()
        );
        // Make two central/local file names collide without changing ZIP offsets.
        let mut bytes = baseline;
        let mut names = inventory().keys().cloned().collect::<Vec<_>>();
        names.sort_by_key(String::len);
        if let Some(pair) = names.windows(2).find(|p| p[0].len() == p[1].len()) {
            for index in 0..=bytes.len() - pair[0].len() {
                if bytes[index..index + pair[0].len()] == *pair[1].as_bytes() {
                    bytes[index..index + pair[0].len()].copy_from_slice(pair[0].as_bytes());
                }
            }
            let duplicate = dir.path().join("duplicate.zip");
            fs::write(&duplicate, bytes).unwrap();
            let stage = tempfile::tempdir_in(dir.path()).unwrap();
            assert!(
                unpack_fixture(&duplicate, stage.path(), total, &CancellationToken::new()).is_err()
            );
        } else {
            panic!("inventory fixture needs equal-length filename pair");
        }
    }
    #[test]
    fn header_rejects_count_comment_trailing_bytes_and_zip64_budget() {
        let dir = test_support::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        tiny_archive(&path, None, false, true);
        assert!(bounded_zip_header(&mut File::open(&path).unwrap(), inventory().len()).is_err());
        tiny_archive(&path, None, false, false);
        let baseline = fs::read(&path).unwrap();
        for mutation in 0..4 {
            let mut bytes = baseline.clone();
            let end = bytes.len() - 22;
            match mutation {
                0 => bytes[end + 10] = 0,
                1 => bytes.push(0),
                2 => bytes[end + 12..end + 16].copy_from_slice(&u32::MAX.to_le_bytes()),
                _ => bytes[..4].copy_from_slice(b"MZxx"),
            };
            fs::write(&path, bytes).unwrap();
            assert!(
                bounded_zip_header(&mut File::open(&path).unwrap(), inventory().len()).is_err()
            );
        }
    }
    #[test]
    fn cancelled_extraction_and_hash_mismatch_publish_no_files() {
        let dir = test_support::tempdir().unwrap();
        let path = dir.path().join("pack.zip");
        let (length, _, total) = tiny_archive(&path, None, false, false);
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(unpack_fixture(&path, stage.path(), total, &cancel).is_err());
        assert_eq!(fs::read_dir(stage.path()).unwrap().count(), 0);
        assert!(
            unpack_zip(
                &path,
                stage.path(),
                length,
                &"0".repeat(64),
                total,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(stage.path()).unwrap().count(), 0);
    }
    fn pointer_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        InstalledModel,
        InstalledModel,
    ) {
        let dir = test_support::tempdir().unwrap();
        let root = dir.path().join("ai-model");
        fs::create_dir(&root).unwrap();
        let updates = dir.path().join("updates");
        fs::create_dir(&updates).unwrap();
        let old = InstalledModel::legacy();
        fs::create_dir(old.directory(&root).unwrap()).unwrap();
        fs::write(
            old.directory(&root).unwrap().join("manifest.json"),
            b"legacy fixture",
        )
        .unwrap();
        let next = InstalledModel {
            model_id: MODEL_ID.into(),
            release_id: 1,
            package_sha256: Some("a".repeat(64)),
            profile: model::fingerprint(),
        };
        (dir, root, updates, old, next)
    }
    #[test]
    fn consent_and_cancellation_rechecked_at_pointer_boundary() {
        let (dir, root, updates, old, next) = pointer_fixture();
        let mut preferences = Preferences {
            automatic_installed_model_updates: true,
            ..Preferences::default()
        };
        preferences
            .save(&dir.path().join("update-settings.json"))
            .unwrap();
        let gate = Mutex::new(false);
        let cancel = CancellationToken::new();
        assert!(publish_activation(&root, &updates, &old, &next, false, &cancel, &gate).is_err());
        preferences.automatic_installed_model_updates = false;
        preferences
            .save(&dir.path().join("update-settings.json"))
            .unwrap();
        assert!(publish_activation(&root, &updates, &old, &next, true, &cancel, &gate).is_err());
        preferences.automatic_installed_model_updates = true;
        preferences
            .save(&dir.path().join("update-settings.json"))
            .unwrap();
        cancel.cancel();
        assert!(publish_activation(&root, &updates, &old, &next, true, &cancel, &gate).is_err());
        *gate.lock().unwrap() = true;
        assert!(
            publish_activation(
                &root,
                &updates,
                &old,
                &next,
                true,
                &CancellationToken::new(),
                &gate
            )
            .is_err()
        );
        assert_eq!(installed(&root).unwrap(), Some(old.clone()));
        *gate.lock().unwrap() = false;
        publish_activation(
            &root,
            &updates,
            &old,
            &next,
            true,
            &CancellationToken::new(),
            &gate,
        )
        .unwrap();
        assert_eq!(installed(&root).unwrap(), Some(next.clone()));
        assert_eq!(pointer(&root).unwrap().unwrap().previous, Some(old.clone()));
        assert!(old.directory(&root).unwrap().join("manifest.json").exists());
        assert!(
            publish_activation(
                &root,
                &updates,
                &old,
                &next,
                true,
                &CancellationToken::new(),
                &gate
            )
            .is_err()
        );
    }
    #[test]
    fn tombstone_prevents_legacy_resurrection_and_pointer_rejects_paths() {
        let (_dir, root, _updates, old, mut next) = pointer_fixture();
        assert_eq!(installed(&root).unwrap(), Some(old));
        unregister(&root).unwrap();
        assert_eq!(installed(&root).unwrap(), None);
        next.package_sha256 = Some("../../other".into());
        assert!(next.directory(&root).is_err());
        fs::write(
            root.join(POINTER_FILE),
            br#"{"schema":99,"current":null,"previous":null}"#,
        )
        .unwrap();
        assert!(installed(&root).is_err());
    }
    #[test]
    fn paused_job_drop_retains_pause_intent_and_cancel_reverses_it() {
        let observed = Arc::new(AtomicBool::new(false));
        let worker_observed = observed.clone();
        let job = ModelJob::spawn(move |cancel, _gate, keep| {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            worker_observed.store(keep.load(Ordering::Acquire), Ordering::Release);
            Ok(None)
        });
        job.pause();
        assert!(job.keep_partial.load(Ordering::Acquire));
        drop(job);
        assert!(observed.load(Ordering::Acquire));
        let job = ModelJob::spawn(move |cancel, _gate, _keep| {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(None)
        });
        job.pause();
        job.cancel();
        assert!(!job.keep_partial.load(Ordering::Acquire));
        drop(job);
    }
    #[test]
    fn explicit_cancel_cleans_only_exact_partial_files_after_lock_release() {
        let dir = test_support::tempdir().unwrap();
        let archive = dir.path().join("1-trusted.zip");
        for name in [
            "1-trusted.zip.part",
            "1-trusted.zip.part.json",
            "other.zip.part",
            "manual.json",
        ] {
            fs::write(dir.path().join(name), b"fixture").unwrap();
        }
        let lock = storage::LockGuard::acquire(&archive.with_extension("zip.download.lock"), false)
            .unwrap();
        assert!(clear_cancelled_partial(&archive).is_err());
        assert!(dir.path().join("1-trusted.zip.part").exists());
        drop(lock);
        clear_cancelled_partial(&archive).unwrap();
        assert!(!dir.path().join("1-trusted.zip.part").exists());
        assert!(!dir.path().join("1-trusted.zip.part.json").exists());
        assert!(dir.path().join("other.zip.part").exists());
        assert!(dir.path().join("manual.json").exists());
    }
    #[test]
    fn promotion_never_overwrites_and_profile_never_changes_quality() {
        let (dir, _root, _updates, old, next) = pointer_fixture();
        let source = dir.path().join("stage");
        let target = dir.path().join("version");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("x"), b"new").unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("x"), b"old").unwrap();
        assert!(promote_directory(&source, &target).is_err());
        assert_eq!(fs::read(target.join("x")).unwrap(), b"old");
        let before = compatibility(&old, "id".into()).unwrap();
        let after = compatibility(&next, "id".into()).unwrap();
        assert_eq!(before.pack_fingerprint, after.pack_fingerprint);
        assert_ne!(old.revision(), next.revision());
        assert!(!model::stable_approved());
    }
}
