//! Local, bounded checks requested by the user. UI badges never enter image buffers.
pub mod cache;
pub mod jpeg;
pub mod metadata;
pub mod model;
pub mod policy;
pub mod types;
#[cfg(windows)]
pub mod windows;
use crate::updater::model_update::{self, InstalledModel, PreparedModelUpdate};
use crate::{Cancellation, Result, ToolError};
use cache::Cache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};
pub use types::*;
pub const MAX_QUEUE: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    pub schema_version: u8,
    pub auto_scan: bool,
    pub show_badges: bool,
    pub persist_results: bool,
}
impl Default for AiSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            auto_scan: false,
            show_badges: true,
            persist_results: true,
        }
    }
}
/// Kept separate from schema-1 ai-settings.json so rollback cannot reset v0.6
/// preferences. A future/corrupt file is preserved and fails closed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentalSettings {
    schema_version: u8,
    enabled: bool,
    risk_ack_version: u8,
}
impl Default for ExperimentalSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            risk_ack_version: 0,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn is_network_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    if windows::network_path(path) {
        return true;
    }
    value.starts_with("//")
        || value.starts_with("\\\\?\\UNC\\")
        || (value.starts_with("\\\\") && !value.starts_with("\\\\?\\"))
}
fn modified(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| v.as_nanos())
        .unwrap_or_default()
}
fn key(_path: &Path, _file: &File, _meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("unix:{}:{}", _meta.dev(), _meta.ino())
    }
    #[cfg(windows)]
    {
        windows::file_key(_file).unwrap_or_else(|| {
            format!(
                "path:{}",
                _path
                    .canonicalize()
                    .unwrap_or_else(|_| _path.to_path_buf())
                    .to_string_lossy()
            )
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        format!("path:{}", _path.to_string_lossy())
    }
}
fn snapshot(path: &Path, cancel: &Cancellation) -> Result<(AssetIdentity, Vec<u8>)> {
    cancel.check()?;
    let location_before = std::fs::symlink_metadata(path)?;
    if !location_before.is_file() || location_before.file_type().is_symlink() {
        return Err(ToolError::Invalid(
            "不支持：检测仅接受普通文件，不跟随符号链接".into(),
        ));
    }
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > crate::pipeline::MAX_INPUT_BYTES {
        return Err(ToolError::Invalid(
            "不支持：检测仅支持不超过 64 MiB 的普通图片文件".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > crate::pipeline::MAX_INPUT_BYTES as usize {
            return Err(ToolError::Invalid("不支持：读取时文件超过检测上限".into()));
        }
        hasher.update(&buffer[..n]);
        bytes.extend_from_slice(&buffer[..n]);
    }
    let after = file.metadata()?;
    let location_after = std::fs::symlink_metadata(path)?;
    let location_file = File::open(path)?;
    let located = location_file.metadata()?;
    if !location_after.is_file()
        || location_after.file_type().is_symlink()
        || before.len() != after.len()
        || modified(&before) != modified(&after)
        || key(path, &file, &before) != key(path, &location_file, &located)
        || modified(&after) != modified(&located)
        || after.len() != bytes.len() as u64
    {
        return Err(ToolError::Invalid("检测期间文件变化，请刷新重试".into()));
    }
    cancel.check()?;
    Ok((
        AssetIdentity {
            key: key(path, &file, &before),
            content_hash: format!("{:x}", hasher.finalize()),
            size: bytes.len() as u64,
            modified_ns: modified(&after),
        },
        bytes,
    ))
}
struct Request {
    path: PathBuf,
    generation: u64,
    ticket: u64,
    cancel: Cancellation,
    model: Option<PathBuf>,
    cause: QueueCause,
    component_state: ComponentState,
    pack_revision: Option<String>,
}
struct Response {
    path: PathBuf,
    generation: u64,
    ticket: u64,
    result: Result<MachineRecord>,
}
#[derive(Default)]
struct State {
    queue: VecDeque<Request>,
    active: Option<(PathBuf, u64, Cancellation, bool)>,
    paused: bool,
    shutdown: bool,
}
struct Worker {
    state: Arc<(Mutex<State>, Condvar)>,
    receiver: mpsc::Receiver<Response>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    fn new() -> Self {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let child = state.clone();
        let (tx, receiver) = mpsc::sync_channel(MAX_QUEUE);
        let thread = thread::spawn(move || {
            loop {
                let request = {
                    let (lock, wake) = &*child;
                    let mut s = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while !s.shutdown && (s.paused || s.queue.is_empty()) {
                        s = wake.wait(s).unwrap_or_else(|p| p.into_inner());
                    }
                    if s.shutdown {
                        break;
                    }
                    let request = s.queue.pop_front().unwrap();
                    s.active = Some((
                        request.path.clone(),
                        request.ticket,
                        request.cancel.clone(),
                        request.model.is_some(),
                    ));
                    request
                };
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inspect(&request)))
                        .unwrap_or_else(|_| {
                            Err(ToolError::Invalid("检测任务异常，可手动重试".into()))
                        });
                let mut response = Response {
                    path: request.path,
                    generation: request.generation,
                    ticket: request.ticket,
                    result,
                };
                loop {
                    if request.cancel.check().is_err() {
                        break;
                    }
                    match tx.try_send(response) {
                        Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => break,
                        Err(mpsc::TrySendError::Full(returned)) => {
                            response = returned;
                            // Cancellable backpressure: never silently lose a current reply,
                            // and shutdown never waits for the UI to drain the receiver.
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                }
                child.0.lock().unwrap_or_else(|p| p.into_inner()).active = None;
            }
        });
        Self {
            state,
            receiver,
            thread: Some(thread),
        }
    }
    fn submit(&self, r: Request, front: bool) -> bool {
        let mut s = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        s.queue.retain(|request| request.cancel.check().is_ok());
        if s.shutdown || s.queue.len() >= MAX_QUEUE {
            return false;
        }
        if front {
            s.queue.push_front(r)
        } else {
            s.queue.push_back(r)
        }
        self.state.1.notify_one();
        true
    }
    fn prioritize(
        &self,
        visible: &[PathBuf],
        current: Option<&Path>,
        evict: usize,
    ) -> Vec<PathBuf> {
        let mut s = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        let mut removed = Vec::new();
        for _ in 0..evict {
            let Some(index) = s.queue.iter().rposition(|r| {
                Some(r.path.as_path()) != current
                    && !visible.contains(&r.path)
                    && r.cause != QueueCause::UserManual
            }) else {
                break;
            };
            if let Some(request) = s.queue.remove(index) {
                request.cancel.cancel();
                removed.push(request.path);
            }
        }
        s.queue.make_contiguous().sort_by_key(|r| {
            if Some(r.path.as_path()) == current {
                0
            } else {
                visible
                    .iter()
                    .position(|p| *p == r.path)
                    .map_or(usize::MAX, |n| n + 1)
            }
        });
        removed
    }
    fn is_running(&self, path: &Path, ticket: u64) -> bool {
        self.state
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .active
            .as_ref()
            .is_some_and(|(p, t, _, _)| p == path && *t == ticket)
    }
    fn is_pixel_task(&self, path: &Path, ticket: u64) -> bool {
        let state = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((p, t, _, pixel)) = &state.active
            && p == path
            && *t == ticket
        {
            return *pixel;
        }
        state
            .queue
            .iter()
            .find(|r| r.path == path && r.ticket == ticket)
            .is_some_and(|r| r.model.is_some())
    }
    fn cancel(&self) {
        let mut s = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        for r in s.queue.drain(..) {
            r.cancel.cancel();
        }
        if let Some((_, _, c, _)) = &s.active {
            c.cancel();
        }
    }
    fn pause(&self, value: bool) {
        let mut s = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
        s.paused = value;
        self.state.1.notify_one();
    }
    fn shutdown(&mut self) {
        self.cancel();
        {
            let mut s = self.state.0.lock().unwrap_or_else(|p| p.into_inner());
            s.shutdown = true;
            self.state.1.notify_one();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn inspect(request: &Request) -> Result<MachineRecord> {
    let _reservation = crate::memory::reserve(crate::pipeline::MAX_INPUT_BYTES)?;
    let (identity, bytes) = snapshot(&request.path, &request.cancel)?;
    let mut record = MachineRecord {
        identity,
        evidence: Vec::new(),
        phase: Phase::AwaitingModel,
        model: None,
        checked_unix_seconds: now(),
        component_state: Default::default(),
        run_state: None,
        release_tier: Default::default(),
        source_error: None,
        pixel_checked_unix_seconds: None,
        pack_revision: None,
    };
    record.component_state = request.component_state.clone();
    record.pack_revision = request.pack_revision.clone();
    record.release_tier = if request.component_state == ComponentState::Ready {
        ReleaseTier::Experimental
    } else {
        ReleaseTier::Unavailable
    };
    record.run_state = Some(RunState::SourceOnly);
    match metadata::inspect(&bytes, &request.cancel) {
        Ok(e) => record.evidence = e,
        Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
        Err(e) => {
            record.source_error = Some(e.to_string());
            record.phase = Phase::Failed(e.to_string());
            record.run_state = Some(RunState::SourceFailed(e.to_string()));
        }
    }
    if let Some(pack) = &request.model {
        match model::preflight_input(&bytes)
            .and_then(|()| model::infer_in_worker(pack, &bytes, &request.cancel))
        {
            Ok(result) => {
                record.model = Some(result);
                record.pixel_checked_unix_seconds = Some(now());
                record.phase = Phase::Complete;
                record.run_state = Some(RunState::CompletedExperimental);
            }
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Err(e) => {
                let message = e.to_string();
                if matches!(record.phase, Phase::Failed(_)) {
                    if let Phase::Failed(previous) = &mut record.phase {
                        previous.push_str(&format!("；像素检查：{message}"));
                    }
                    request.cancel.check()?;
                } else {
                    record.phase = if message.starts_with("不支持：") {
                        Phase::Unsupported(message.clone())
                    } else if message.contains("像素检测超时") {
                        Phase::TimedOut
                    } else {
                        Phase::Failed(message.clone())
                    };
                }
                record.run_state = Some(match &record.phase {
                    Phase::Unsupported(reason) => RunState::Unsupported(reason.clone()),
                    Phase::TimedOut => RunState::TimedOut,
                    Phase::Failed(reason) => RunState::Failed(reason.clone()),
                    _ => RunState::Failed(message),
                });
            }
        }
    } else if request.component_state == ComponentState::Ready && !record.phase.is_error() {
        record.run_state = Some(RunState::Disabled);
    }
    request.cancel.check()?;
    // A worker may spend seconds on inference. Never publish a score for bytes
    // that changed while it ran, even when the path and length stayed the same.
    drop(bytes);
    let (current_identity, _) = snapshot(&request.path, &request.cancel)?;
    if !record.identity.same_content(&current_identity) {
        record.identity = current_identity;
        record.model = None;
        record.evidence.clear();
        record.phase = Phase::AwaitingModel;
        record.run_state = Some(RunState::Expired);
    }
    Ok(record)
}

struct Installation {
    receiver: mpsc::Receiver<Result<(PathBuf, model::ComponentInfo, InstalledModel)>>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Installation {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
pub struct Controller {
    worker: Worker,
    cache: Cache,
    settings: AiSettings,
    settings_path: Option<PathBuf>,
    settings_writable: bool,
    experimental: ExperimentalSettings,
    experimental_path: Option<PathBuf>,
    experimental_writable: bool,
    component_state: ComponentState,
    component_info: Option<model::ComponentInfo>,
    installed_model: Option<InstalledModel>,
    paths: Vec<PathBuf>,
    scope: HashSet<PathBuf>,
    visible: Vec<PathBuf>,
    current: Option<PathBuf>,
    records: HashMap<PathBuf, MachineRecord>,
    expired_paths: HashSet<PathBuf>,
    manuals: HashMap<String, ManualOverride>,
    outstanding: HashSet<PathBuf>,
    attempted: HashSet<PathBuf>,
    tokens: HashMap<PathBuf, (u64, Cancellation)>,
    generation: u64,
    ticket: u64,
    paused: bool,
    warning: Option<String>,
    model_pack: Option<PathBuf>,
    store_directory: Option<PathBuf>,
    installation: Option<Installation>,
    pending_manual: Option<PathBuf>,
}
impl Controller {
    pub fn new(store_path: Option<PathBuf>) -> Self {
        let settings_path = store_path
            .as_ref()
            .map(|p| p.with_file_name("ai-settings.json"));
        let mut settings = AiSettings::default();
        let mut warning = None;
        let mut settings_writable = true;
        if let Some(path) = &settings_path {
            match File::open(path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    let result = file.take(16385).read_to_end(&mut bytes).and_then(|_| {
                        if bytes.len() > 16384 {
                            Err(std::io::Error::other("AI 设置超过大小限制"))
                        } else {
                            Ok(())
                        }
                    });
                    match result
                        .ok()
                        .and_then(|_| serde_json::from_slice::<AiSettings>(&bytes).ok())
                        .filter(|s| s.schema_version == 1)
                    {
                        Some(s) => settings = s,
                        None => {
                            warning = Some(
                                "AI 设置损坏或版本较新，原文件保留；本次设置仅在会话有效".into(),
                            );
                            settings_writable = false;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warning = Some(format!("AI 设置无法读取：{e}"));
                    settings_writable = false;
                }
            }
        }
        let cache = match store_path
            .as_ref()
            .map(|p| Cache::open(p, settings.persist_results))
        {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                warning = Some(format!("AI 缓存不可用，本次仅保存在内存；原记录保留：{e}"));
                Cache::memory().expect("in-memory SQLite")
            }
            None => Cache::memory().expect("in-memory SQLite"),
        };
        let store_directory = store_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf);
        let mut model_pointer_failure = None;
        let installed_model = store_directory.as_ref().and_then(|directory| {
            match model_update::installed(&directory.join("ai-model")) {
                Ok(value) => value,
                Err(error) => {
                    warning = Some(format!("组件指针无法读取：{error}"));
                    model_pointer_failure = Some(error.to_string());
                    None
                }
            }
        });
        let model_pack = installed_model.as_ref().and_then(|installed| {
            installed
                .directory(&store_directory.as_ref()?.join("ai-model"))
                .ok()
        });
        let experimental_path = store_path
            .as_ref()
            .map(|p| p.with_file_name("ai-experimental-settings.json"));
        let mut experimental = ExperimentalSettings::default();
        let mut experimental_writable = true;
        if let Some(path) = &experimental_path {
            match File::open(path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    let valid =
                        file.take(4097).read_to_end(&mut bytes).is_ok() && bytes.len() <= 4096;
                    match valid
                        .then(|| serde_json::from_slice::<ExperimentalSettings>(&bytes).ok())
                        .flatten()
                        .filter(|v| {
                            v.schema_version == 1 && (!v.enabled || v.risk_ack_version == 1)
                        }) {
                        Some(v) => experimental = v,
                        None => {
                            experimental_writable = false;
                            warning = Some("实验设置无效或版本较新，已保持关闭；原文件保留".into());
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    experimental_writable = false;
                    warning = Some(format!("实验设置无法读取，已保持关闭：{e}"));
                }
            }
        }
        let component_state = if let Some(error) = model_pointer_failure {
            ComponentState::Failed(error)
        } else {
            ComponentState::Missing
        };
        Self {
            worker: Worker::new(),
            cache,
            settings,
            settings_path,
            settings_writable,
            experimental,
            experimental_path,
            experimental_writable,
            component_state,
            component_info: None,
            installed_model,
            paths: Vec::new(),
            scope: HashSet::new(),
            visible: Vec::new(),
            current: None,
            records: HashMap::new(),
            expired_paths: HashSet::new(),
            manuals: HashMap::new(),
            outstanding: HashSet::new(),
            attempted: HashSet::new(),
            tokens: HashMap::new(),
            generation: 0,
            ticket: 0,
            paused: false,
            warning,
            model_pack,
            store_directory,
            installation: None,
            pending_manual: None,
        }
    }
    fn save_settings(&mut self) {
        if !self.settings_writable {
            return;
        }
        let Some(path) = &self.settings_path else {
            return;
        };
        let result = (|| -> Result<()> {
            let parent = path
                .parent()
                .ok_or_else(|| ToolError::Invalid("设置目录无效".into()))?;
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(&mut temp, &self.settings)
                .map_err(|e| ToolError::Invalid(e.to_string()))?;
            temp.write_all(b"\n")?;
            temp.as_file().sync_all()?;
            temp.persist(path).map_err(|e| e.error)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.warning = Some(format!("AI 设置未保存：{e}"));
        }
    }
    fn cancel(&mut self) {
        self.worker.cancel();
        self.pending_manual = None;
        for (_, c) in self.tokens.values() {
            c.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        for path in &self.outstanding {
            self.attempted.remove(path);
        }
        self.outstanding.clear();
        self.tokens.clear();
    }
    fn queue(&mut self, path: PathBuf, cause: QueueCause, front: bool) {
        if self.outstanding.contains(&path) {
            return;
        }
        if front && self.outstanding.len() >= MAX_QUEUE {
            // Current/manual work preempts not-yet-completed background descriptors.
            // Valid already-published results remain, canceled paths can be refilled.
            self.cancel();
        }
        if self.outstanding.len() >= MAX_QUEUE {
            return;
        }
        let cancel = Cancellation::default();
        self.ticket = self.ticket.wrapping_add(1);
        let request = Request {
            path: path.clone(),
            generation: self.generation,
            ticket: self.ticket,
            cancel: cancel.clone(),
            model: if self.component_state == ComponentState::Ready
                && cause == QueueCause::UserManual
            {
                self.model_pack.clone()
            } else {
                None
            },
            cause,
            component_state: self.component_state.clone(),
            pack_revision: self
                .installed_model
                .as_ref()
                .and_then(InstalledModel::revision),
        };
        if self.worker.submit(request, front) {
            self.outstanding.insert(path.clone());
            self.attempted.insert(path.clone());
            self.tokens.insert(path, (self.ticket, cancel));
        }
    }
    pub fn prioritize_visible(&mut self, paths: &[PathBuf]) {
        let visible: Vec<_> = paths
            .iter()
            .filter(|p| self.scope.contains(*p))
            .take(MAX_QUEUE - 1)
            .cloned()
            .collect();
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.worker
            .prioritize(&self.visible, self.current.as_deref(), 0);
    }
    pub fn set_scope(&mut self, paths: &[PathBuf], current: Option<&Path>) {
        self.set_scanned_scope(paths, current, &HashMap::new());
    }
    /// Directory metadata is gathered by the bounded scan worker. This method
    /// only changes view scope; explicit checks revalidate input at snapshot time.
    pub fn set_scanned_scope(
        &mut self,
        paths: &[PathBuf],
        current: Option<&Path>,
        metadata: &HashMap<PathBuf, crate::browsing::FileMetadata>,
    ) {
        // Scope changes only update records and navigation; they do not request checks.
        let parent = current
            .and_then(Path::parent)
            .or_else(|| paths.first().and_then(|p| p.parent()));
        let filtered: Vec<_> = paths
            .iter()
            .filter(|p| p.parent() == parent)
            .filter(|p| {
                metadata
                    .get(*p)
                    .is_none_or(|m| m.regular_file && !m.symlink)
            })
            .take(crate::browsing::MAX_DIRECTORY_IMAGES)
            .cloned()
            .collect();
        let paths = filtered.as_slice();
        if self.current.as_deref() != current
            || !current.is_some_and(|path| paths.iter().any(|candidate| candidate == path))
        {
            self.cancel();
        }
        if self.paths != paths {
            self.paths = paths
                .iter()
                .take(crate::browsing::MAX_DIRECTORY_IMAGES)
                .cloned()
                .collect();
            self.scope = self.paths.iter().cloned().collect();
            self.visible.retain(|p| self.scope.contains(p));
            self.records.retain(|p, _| self.scope.contains(p));
            self.expired_paths.retain(|p| self.scope.contains(p));
            self.attempted.retain(|p| self.scope.contains(p));
            let keys: HashSet<_> = self
                .records
                .values()
                .map(|r| r.identity.key.clone())
                .collect();
            self.manuals.retain(|key, _| keys.contains(key));
        }
        self.current = current.map(Path::to_path_buf);
        if current.is_some_and(is_network_path) {
            self.paths = current.iter().map(|p| p.to_path_buf()).collect();
            self.scope = self.paths.iter().cloned().collect();
            self.warning = Some("网络路径仅检查显式打开的当前文件，不自动分析共享目录".into());
        }
    }
    pub fn set_current(&mut self, path: &Path) {
        if self.current.as_deref() != Some(path) {
            self.cancel();
        } else {
            self.pending_manual = None;
        }
        self.current = Some(path.to_path_buf());
        // A reopen is a content-validation boundary even at the same path.
        // Remove old badges/overrides from effective view until bytes revalidate.
        if self.records.remove(path).is_some_and(|r| r.model.is_some()) {
            self.expired_paths.insert(path.to_path_buf());
        }
        if let Some((_, token)) = self.tokens.remove(path) {
            token.cancel();
        }
        self.outstanding.remove(path);
        self.attempted.remove(path);
    }
    pub fn settings(&self) -> &AiSettings {
        &self.settings
    }
    pub fn set_auto_scan(&mut self, value: bool) {
        self.settings.auto_scan = value;
        if !value {
            let pending: Vec<_> = self.outstanding.iter().cloned().collect();
            self.cancel();
            for p in pending {
                self.attempted.remove(&p);
            }
        }
        self.save_settings();
    }
    pub fn set_show_badges(&mut self, value: bool) {
        self.settings.show_badges = value;
        self.save_settings();
    }
    pub fn set_persist_results(&mut self, value: bool) {
        match self.cache.set_persistent(value) {
            Ok(()) => {
                self.settings.persist_results = value;
                self.save_settings();
            }
            Err(e) => self.warning = Some(e.to_string()),
        }
    }
    pub fn pause(&mut self, value: bool) {
        self.paused = value;
        self.worker.pause(value);
    }
    pub fn is_paused(&self) -> bool {
        self.paused
    }
    pub fn is_busy(&self) -> bool {
        self.installation.is_some() || (!self.paused && !self.outstanding.is_empty())
    }
    pub fn inspect_source_current(&mut self, path: &Path) {
        self.cancel();
        self.worker.pause(false);
        self.paused = false;
        self.queue(path.to_path_buf(), QueueCause::SourceOnly, true);
    }
    pub fn scan_current(&mut self, path: &Path) {
        self.cancel();
        self.worker.pause(false);
        self.paused = false;
        if self.component_state == ComponentState::Ready && self.model_pack.is_some() {
            self.queue(path.to_path_buf(), QueueCause::UserManual, true);
        } else if self.installation.is_some() {
            self.pending_manual = Some(path.to_path_buf());
        } else if self.has_installed_model() {
            self.pending_manual = Some(path.to_path_buf());
            let root = self.store_directory.as_ref().unwrap().join("ai-model");
            let (tx, receiver) = mpsc::sync_channel(1);
            let thread = thread::spawn(move || {
                let result = model_update::validate_installed_or_recover(&root)
                    .map_err(|e| ToolError::Invalid(e.to_string()))
                    .and_then(|value| value.ok_or_else(|| ToolError::Invalid("组件已移除".into())));
                let _ = tx.try_send(result);
            });
            self.component_state = ComponentState::Checking;
            self.installation = Some(Installation {
                receiver,
                thread: Some(thread),
            });
        } else {
            self.warning =
                Some("尚未安装 AI 检测组件；请先在组件管理中离线导入，当前图片未检测".into());
        }
    }
    pub fn refresh(&mut self) {
        self.cancel();
        self.attempted.clear();
        self.records.clear();
    }
    pub fn retry_errors(&mut self) {
        let paths: Vec<_> = self
            .records
            .iter()
            .filter(|(_, r)| matches!(r.phase, Phase::Failed(_) | Phase::TimedOut))
            .map(|(p, _)| p.clone())
            .collect();
        for p in paths {
            self.attempted.remove(&p);
            // This button is an explicit user request, independent of automatic
            // preferences. A retry may use the already verified component.
            self.queue(p, QueueCause::UserManual, false);
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(result) =
            self.installation
                .as_ref()
                .and_then(|task| match task.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(Err(ToolError::Invalid(
                        "组件校验线程异常退出，可重试".into(),
                    ))),
                })
        {
            self.installation.take();
            match result {
                Ok((path, info, installed)) => {
                    self.installed_model = Some(installed);
                    self.model_pack = Some(path);
                    self.component_info = Some(info);
                    self.component_state = ComponentState::Ready;
                    self.warning =
                        Some("实验组件已校验并通过固定自检；尚未通过正式发布验证标准".into());
                    if let Some(path) = self.pending_manual.take()
                        && self.current.as_ref() == Some(&path)
                    {
                        self.queue(path, QueueCause::UserManual, true);
                    }
                }
                Err(error) => {
                    self.pending_manual = None;
                    self.component_state = ComponentState::Failed(error.to_string());
                    self.warning = Some(format!("组件校验或自检失败：{error}"));
                }
            }
            changed = true;
        }
        let frame_started = std::time::Instant::now();
        for _ in 0..4 {
            if frame_started.elapsed() > std::time::Duration::from_millis(8) {
                break;
            }
            let Ok(response) = self.worker.receiver.try_recv() else {
                break;
            };
            if response.generation != self.generation {
                continue;
            }
            if self
                .tokens
                .get(&response.path)
                .is_none_or(|(ticket, _)| *ticket != response.ticket)
            {
                continue;
            }
            let Some((_, token)) = self.tokens.remove(&response.path) else {
                continue;
            };
            self.outstanding.remove(&response.path);
            if token.check().is_err() {
                continue;
            }
            match response.result {
                Ok(mut record) => {
                    self.expired_paths.remove(&response.path);
                    let mut preserve_cached_model = false;
                    if record.model.is_none()
                        && matches!(
                            record.run_state,
                            Some(RunState::SourceOnly | RunState::Disabled)
                        )
                        && let Ok(Some(cached)) = self.cache.get_machine(&record.identity)
                        && let Some(mut model) = cached.model
                    {
                        if self.component_state == ComponentState::Ready
                            && model.fingerprint == model::fingerprint()
                            && cached.run_state == Some(RunState::CompletedExperimental)
                            && cached.pack_revision
                                == self
                                    .installed_model
                                    .as_ref()
                                    .and_then(InstalledModel::revision)
                            && cached.phase == Phase::Complete
                            && cached.release_tier == ReleaseTier::Experimental
                            && model.threshold == model::RESEARCH_THRESHOLD
                        {
                            model.approved = false;
                            model.high_signal = false;
                            record.model = Some(model);
                            record.pack_revision = cached.pack_revision;
                            record.pixel_checked_unix_seconds = cached.pixel_checked_unix_seconds;
                            record.component_state = ComponentState::Ready;
                            record.release_tier = ReleaseTier::Experimental;
                            record.phase = Phase::Complete;
                            record.run_state = Some(RunState::CompletedExperimental);
                        } else {
                            record.run_state = Some(RunState::Expired);
                            preserve_cached_model = true;
                        }
                    }
                    if let Ok(Some(manual)) = self.cache.get_manual(&record.identity.key) {
                        self.manuals.insert(record.identity.key.clone(), manual);
                    }
                    if !preserve_cached_model && let Err(e) = self.cache.put_machine(&record) {
                        self.warning = Some(format!("结果仅保留本次会话：{e}"));
                    }
                    self.records.insert(response.path, record);
                }
                Err(ToolError::Cancelled) => {
                    self.attempted.remove(&response.path);
                }
                Err(error) => {
                    let message = error.to_string();
                    let (phase, run_state) = if message.starts_with("不支持：") {
                        (
                            Phase::Unsupported(message.clone()),
                            RunState::Unsupported(message),
                        )
                    } else if message.contains("检测期间文件变化") {
                        (Phase::AwaitingModel, RunState::Expired)
                    } else {
                        (Phase::Failed(message.clone()), RunState::Failed(message))
                    };
                    self.records.insert(
                        response.path,
                        MachineRecord {
                            identity: AssetIdentity {
                                key: String::new(),
                                content_hash: String::new(),
                                size: 0,
                                modified_ns: 0,
                            },
                            evidence: Vec::new(),
                            phase,
                            model: None,
                            checked_unix_seconds: now(),
                            component_state: self.component_state.clone(),
                            run_state: Some(run_state),
                            release_tier: self.release_tier(),
                            source_error: None,
                            pixel_checked_unix_seconds: None,
                            pack_revision: None,
                        },
                    );
                }
            }
            changed = true;
        }
        changed
    }
    pub fn record(&self, path: &Path) -> Option<&MachineRecord> {
        self.records.get(path)
    }
    pub fn manual(&self, path: &Path) -> Option<&ManualOverride> {
        self.records
            .get(path)
            .and_then(|r| self.manuals.get(&r.identity.key))
    }
    pub fn run_state(&self, path: &Path) -> RunState {
        if self.pending_manual.as_deref() == Some(path) {
            return RunState::Queued;
        }
        if let Some((ticket, _)) = self.tokens.get(path) {
            let pixel = self.worker.is_pixel_task(path, *ticket);
            return if self.worker.is_running(path, *ticket) {
                if pixel {
                    RunState::Running
                } else {
                    RunState::SourceRunning
                }
            } else if self.paused {
                RunState::Paused
            } else if pixel {
                RunState::Queued
            } else {
                RunState::SourceQueued
            };
        }
        if self.expired_paths.contains(path) {
            return RunState::Expired;
        }
        self.record(path)
            .map(|r| {
                r.run_state.clone().unwrap_or_else(|| match &r.phase {
                    Phase::Unscanned => RunState::Unscanned,
                    Phase::Queued => RunState::Queued,
                    Phase::Running => RunState::Running,
                    Phase::Paused => RunState::Paused,
                    Phase::Failed(reason) => RunState::Failed(reason.clone()),
                    Phase::TimedOut => RunState::TimedOut,
                    Phase::Unsupported(reason) => RunState::Unsupported(reason.clone()),
                    _ => RunState::SourceOnly,
                })
            })
            .unwrap_or(RunState::Unscanned)
    }
    pub fn decision(&self, path: &Path) -> Decision {
        let mut d = policy::decide(self.record(path), self.manual(path));
        if d.bucket == Bucket::Pending {
            let state = self.run_state(path);
            if self.outstanding.contains(path) || state == RunState::Expired {
                d.label = state.label().into();
            } else if self.component_state == ComponentState::Missing && !self.has_installed_model()
            {
                d.label = self.component_state.label().into();
            } else if self.component_state == ComponentState::Checking {
                d.label = "正在校验 AI 组件".into();
            } else if self.component_state == ComponentState::Ready || self.has_installed_model() {
                d.label = "尚未检测".into();
            }
        }
        d
    }
    pub fn mark(&mut self, path: &Path, decision: Option<ManualDecision>) -> Result<()> {
        let record = self
            .records
            .get(path)
            .filter(|r| !r.identity.content_hash.is_empty())
            .ok_or_else(|| ToolError::Invalid("先检测此图片以确认内容身份".into()))?;
        let key = record.identity.key.clone();
        if let Some(decision) = decision {
            let m = ManualOverride {
                content_hash: record.identity.content_hash.clone(),
                decision,
                created_unix_seconds: now(),
            };
            self.cache
                .set_manual(&key, &m)
                .map_err(|e| ToolError::Invalid(e.to_string()))?;
            self.manuals.insert(key, m);
        } else {
            self.cache
                .remove_manual(&key)
                .map_err(|e| ToolError::Invalid(e.to_string()))?;
            self.manuals.remove(&key);
        }
        Ok(())
    }
    pub fn clear_automatic(&mut self) -> Result<()> {
        self.cancel();
        // Clearing is a stop boundary; canceled responses and ordinary browsing
        // cannot silently recreate records until another explicit check.
        self.attempted = self.paths.iter().cloned().collect();
        self.expired_paths.clear();
        self.cache
            .clear_automatic()
            .map_err(|e| ToolError::Invalid(e.to_string()))?;
        for r in self.records.values_mut() {
            r.evidence.clear();
            r.source_error = None;
            r.pixel_checked_unix_seconds = None;
            r.model = None;
            r.phase = Phase::Unscanned;
            r.run_state = Some(RunState::Unscanned);
        }
        Ok(())
    }
    pub fn clear_manual(&mut self) -> Result<()> {
        self.cache
            .clear_manual()
            .map_err(|e| ToolError::Invalid(e.to_string()))?;
        self.manuals.clear();
        Ok(())
    }
    pub fn status(&self) -> String {
        let done = self.records.len();
        let base = if self.paused {
            format!("已暂停 · {done}/{} 项已检查", self.paths.len())
        } else {
            let pixels = self
                .records
                .values()
                .filter(|r| {
                    matches!(
                        r.run_state,
                        Some(RunState::CompletedExperimental | RunState::CompletedStable)
                    )
                })
                .count();
            format!(
                "来源 {done}/{} · 像素完成 {pixels} · 按需检测",
                self.paths.len()
            )
        };
        if let Some(w) = &self.warning {
            format!("{base} · {w}")
        } else {
            base
        }
    }
    pub fn component_state(&self) -> &ComponentState {
        &self.component_state
    }
    pub fn has_installed_model(&self) -> bool {
        self.installed_model.is_some()
            && self.model_pack.is_some()
            && self.store_directory.is_some()
    }
    pub fn component_info(&self) -> Option<&model::ComponentInfo> {
        self.component_info.as_ref()
    }
    pub fn release_tier(&self) -> ReleaseTier {
        if self.component_state == ComponentState::Ready {
            ReleaseTier::Experimental
        } else {
            ReleaseTier::Unavailable
        }
    }
    pub fn model_status(&self) -> &'static str {
        if self.component_state == ComponentState::Missing && self.has_installed_model() {
            "组件已安装 · 点击 AI 检测时校验"
        } else {
            self.component_state.label()
        }
    }
    pub fn experimental_auto_scan(&self) -> bool {
        // Legacy preferences remain on disk for rollback, but browsing never enables detection.
        false
    }
    pub fn set_experimental_auto_scan(
        &mut self,
        enabled: bool,
        _risk_acknowledged: bool,
    ) -> Result<()> {
        if enabled {
            return Err(ToolError::Invalid(
                "自动筛查已停用，请使用 AI 检测按钮检测当前图片".into(),
            ));
        }
        self.cancel();
        self.experimental.enabled = enabled;
        self.experimental.risk_ack_version = if enabled {
            1
        } else {
            self.experimental.risk_ack_version
        };
        self.attempted.clear();
        if self.experimental_writable
            && let Some(path) = &self.experimental_path
        {
            let parent = path
                .parent()
                .ok_or_else(|| ToolError::Invalid("实验设置目录无效".into()))?;
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(&mut temp, &self.experimental)
                .map_err(|e| ToolError::Invalid(e.to_string()))?;
            temp.write_all(b"\n")?;
            temp.as_file().sync_all()?;
            temp.persist(path).map_err(|e| e.error)?;
        }
        Ok(())
    }
    /// Available only after actual hash validation and the fixed self-test.
    pub fn installed_model_for_update(&self) -> Option<InstalledModel> {
        (self.component_state == ComponentState::Ready)
            .then(|| self.installed_model.clone())
            .flatten()
    }
    pub fn model_update_root(&self) -> Option<PathBuf> {
        self.store_directory.as_ref().map(|p| p.join("ai-model"))
    }
    pub fn activate_prepared_model(
        &mut self,
        prepared: PreparedModelUpdate,
        currently_allowed: bool,
    ) -> Result<()> {
        if self.installation.is_some()
            || self.component_state != ComponentState::Ready
            || self.model_update_root().as_deref() != Some(prepared.model_root())
        {
            return Err(ToolError::Invalid(
                "模型组件状态已变化，丢弃更新准备结果".into(),
            ));
        }
        self.cancel();
        self.worker.shutdown();
        let result = prepared.activate(currently_allowed);
        self.worker = Worker::new();
        self.worker.pause(self.paused);
        let (path, info, installed) = result.map_err(|e| ToolError::Invalid(e.to_string()))?;
        self.model_pack = Some(path);
        self.component_info = Some(info);
        self.installed_model = Some(installed);
        self.component_state = ComponentState::Ready;
        for record in self.records.values_mut() {
            if record.model.is_some() {
                record.model = None;
                record.run_state = Some(RunState::Expired);
                record.phase = Phase::AwaitingModel;
                record.pixel_checked_unix_seconds = None;
            }
        }
        self.attempted.clear();
        self.warning = Some("模型组件更新已校验并启用；仍为实验结果，原组件保留供恢复".into());
        Ok(())
    }
    pub fn import_model_pack(&mut self, source: &Path) -> Result<()> {
        let root = self
            .model_update_root()
            .ok_or_else(|| ToolError::Invalid("临时测试会话不安装组件".into()))?;
        if self.installation.is_some() {
            return Err(ToolError::Invalid("组件导入仍在进行".into()));
        }
        if model_update::installed(&root)
            .map_err(|e| ToolError::Invalid(e.to_string()))?
            .is_some()
        {
            return Err(ToolError::Invalid(
                "组件已安装，请先明确移除再离线导入".into(),
            ));
        }
        let source = source.to_path_buf();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let result = model_update::import_offline(&source, &root)
                .map_err(|e| ToolError::Invalid(e.to_string()));
            let _ = tx.try_send(result);
        });
        self.cancel();
        self.component_state = ComponentState::Checking;
        self.installation = Some(Installation {
            receiver,
            thread: Some(thread),
        });
        Ok(())
    }
    pub fn remove_model(&mut self) -> Result<()> {
        if self.installation.is_some() {
            return Err(ToolError::Invalid("组件导入中，请完成后删除".into()));
        }
        self.cancel();
        // The Windows worker holds verified runtime/decoder handles. Wait for
        // cancellation to close them before deleting a component; never race a
        // live inference or leave the UI pointing at half-removed files.
        self.worker.shutdown();
        self.worker = Worker::new();
        self.worker.pause(self.paused);
        let paths = if let Some(root) = self.model_update_root() {
            model_update::unregister(&root).map_err(|e| ToolError::Invalid(e.to_string()))?
        } else {
            self.model_pack.iter().cloned().collect()
        };
        // The tombstone wins even if a retained file cannot be deleted.
        let mut cleanup_error = None;
        for path in paths {
            if path.exists()
                && let Err(error) = model::remove_pack(&path)
            {
                cleanup_error = Some(error);
            }
        }
        self.installed_model = None;
        self.model_pack = None;
        self.component_state = ComponentState::Missing;
        self.component_info = None;
        self.set_experimental_auto_scan(false, false)?;
        for r in self.records.values_mut() {
            r.model = None;
            r.component_state = ComponentState::Missing;
            r.release_tier = ReleaseTier::Unavailable;
            r.run_state = Some(RunState::Expired);
            if !matches!(r.phase, Phase::Failed(_)) {
                r.phase = Phase::AwaitingModel;
            }
        }
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        self.cancel();
        self.worker.shutdown();
        self.installation.take();
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn image(path: &Path) {
        image::RgbImage::from_pixel(240, 240, image::Rgb([20, 30, 40]))
            .save(path)
            .unwrap();
    }
    fn pump(c: &mut Controller) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while c.is_busy() && std::time::Instant::now() < end {
            c.poll();
            thread::sleep(Duration::from_millis(2));
        }
        c.poll();
        assert!(!c.is_busy());
    }
    #[test]
    fn missing_model_is_pending_and_manual_persists_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        image(&p);
        let mut c = Controller::new(None);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.inspect_source_current(&p);
        pump(&mut c);
        assert_eq!(c.decision(&p).bucket, Bucket::Pending);
        c.mark(&p, Some(ManualDecision::Ai)).unwrap();
        assert_eq!(c.decision(&p).bucket, Bucket::Ai);
        c.set_show_badges(false);
        assert_eq!(c.decision(&p).bucket, Bucket::Ai);
        c.clear_automatic().unwrap();
        assert_eq!(c.decision(&p).bucket, Bucket::Ai);
    }
    #[test]
    fn scanned_scope_uses_supplied_file_types_without_touching_nonexistent_paths() {
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("not-yet-created.png");
        let link = dir.path().join("link.png");
        let folder = dir.path().join("folder.png");
        let paths = vec![regular.clone(), link.clone(), folder.clone()];
        let metadata = HashMap::from([
            (
                regular.clone(),
                crate::browsing::FileMetadata {
                    regular_file: true,
                    ..Default::default()
                },
            ),
            (
                link,
                crate::browsing::FileMetadata {
                    symlink: true,
                    ..Default::default()
                },
            ),
            (folder, crate::browsing::FileMetadata::default()),
        ]);
        let mut controller = Controller::new(None);
        controller.set_scanned_scope(&paths, Some(&regular), &metadata);
        assert_eq!(controller.paths, vec![regular.clone()]);
        assert!(controller.scope.contains(&regular));
        assert!(!controller.is_busy());
        assert!(controller.records.is_empty());
    }

    #[test]
    fn explicit_source_check_revalidates_file_kind_at_the_snapshot_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changed-to-directory.png");
        let mut controller = Controller::new(None);
        controller.set_scanned_scope(std::slice::from_ref(&path), Some(&path), &HashMap::new());
        std::fs::create_dir(&path).unwrap();
        controller.inspect_source_current(&path);
        pump(&mut controller);
        assert_eq!(controller.decision(&path).bucket, Bucket::Unsupported);
        assert!(
            controller
                .record(&path)
                .unwrap()
                .identity
                .content_hash
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_source_check_rejects_a_symlink_added_after_scope_scan() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.png");
        let link = dir.path().join("link.png");
        image(&target);
        let mut controller = Controller::new(None);
        controller.set_scanned_scope(std::slice::from_ref(&link), Some(&link), &HashMap::new());
        std::os::unix::fs::symlink(&target, &link).unwrap();
        controller.inspect_source_current(&link);
        pump(&mut controller);
        assert_eq!(controller.decision(&link).bucket, Bucket::Unsupported);
        assert!(
            controller
                .record(&link)
                .unwrap()
                .identity
                .content_hash
                .is_empty()
        );
    }
    #[test]
    fn cancelled_folder_cannot_publish_and_queue_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..1000)
            .map(|i| dir.path().join(format!("{i}.png")))
            .collect();
        let mut c = Controller::new(None);
        c.pause(true);
        c.set_scope(&paths, Some(&paths[0]));
        for p in &paths {
            c.queue(p.clone(), QueueCause::SourceOnly, false);
        }
        assert!(c.outstanding.len() <= MAX_QUEUE);
        c.set_scope(&[], None);
        c.pause(false);
        pump(&mut c);
        assert!(c.records.is_empty());
    }
    #[test]
    fn content_change_stales_manual_even_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        image(&p);
        let mut c = Controller::new(Some(dir.path().join("cache.sqlite")));
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.inspect_source_current(&p);
        pump(&mut c);
        c.mark(&p, Some(ManualDecision::NotAi)).unwrap();
        image::RgbImage::from_pixel(240, 240, image::Rgb([90, 0, 0]))
            .save(&p)
            .unwrap();
        c.refresh();
        c.inspect_source_current(&p);
        pump(&mut c);
        assert_eq!(c.decision(&p).bucket, Bucket::Pending);
        assert!(!c.decision(&p).manual_active);
        assert!(c.manual(&p).is_some());
    }
    #[test]
    fn source_bytes_never_change_and_no_model_no_pixel_claim() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        image(&p);
        let before = std::fs::read(&p).unwrap();
        let mut c = Controller::new(None);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        pump(&mut c);
        c.inspect_source_current(&p);
        pump(&mut c);
        assert!(c.record(&p).unwrap().model.is_none());
        assert_eq!(std::fs::read(p).unwrap(), before);
    }
}

#[cfg(test)]
mod concurrency_regressions {
    use super::*;
    use std::time::{Duration, Instant};
    fn image(path: &Path) {
        image::RgbImage::from_pixel(224, 224, image::Rgb([40, 50, 60]))
            .save(path)
            .unwrap();
    }
    fn pump(c: &mut Controller) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.is_busy() && Instant::now() < deadline {
            c.poll();
            thread::sleep(Duration::from_millis(2));
        }
        c.poll();
        assert!(!c.is_busy(), "{}", c.status());
    }
    #[test]
    fn same_current_scope_refresh_preserves_explicit_request_without_starting_others() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        image(&a);
        image(&b);
        let mut c = Controller::new(None);
        c.pause(true);
        c.set_scope(&[a.clone(), b], Some(&a));
        c.queue(a.clone(), QueueCause::SourceOnly, true);
        c.set_scope(std::slice::from_ref(&a), Some(&a));
        c.pause(false);
        pump(&mut c);
        assert!(c.record(&a).is_some());
    }
    #[test]
    fn same_path_reopen_hides_manual_until_content_reverified_even_auto_off() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(None);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.inspect_source_current(&p);
        pump(&mut c);
        c.mark(&p, Some(ManualDecision::Ai)).unwrap();
        c.set_auto_scan(false);
        image::RgbImage::from_pixel(224, 224, image::Rgb([90, 0, 0]))
            .save(&p)
            .unwrap();
        c.set_current(&p);
        assert_eq!(c.decision(&p).bucket, Bucket::Pending);
        assert!(!c.decision(&p).manual_active);
        c.inspect_source_current(&p);
        pump(&mut c);
        assert!(!c.decision(&p).manual_active);
    }
    #[test]
    fn cancelled_old_ticket_cannot_consume_replacement_request() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let (identity, _) = snapshot(&p, &Cancellation::default()).unwrap();
        let mut c = Controller::new(None);
        let (tx, rx) = mpsc::sync_channel(4);
        c.worker.receiver = rx;
        c.tokens.insert(p.clone(), (2, Cancellation::default()));
        c.outstanding.insert(p.clone());
        for ticket in [1, 2] {
            tx.send(Response {
                path: p.clone(),
                generation: c.generation,
                ticket,
                result: Ok(MachineRecord {
                    identity: identity.clone(),
                    evidence: Vec::new(),
                    phase: Phase::AwaitingModel,
                    model: None,
                    checked_unix_seconds: now(),
                    component_state: Default::default(),
                    run_state: None,
                    release_tier: Default::default(),
                    source_error: None,
                    pixel_checked_unix_seconds: None,
                    pack_revision: None,
                }),
            })
            .unwrap();
        }
        c.poll();
        assert!(c.record(&p).is_some());
        assert!(!c.is_busy());
    }
    #[test]
    fn visible_rows_only_reorder_existing_requests_without_starting_detections() {
        let mut c = Controller::new(None);
        c.pause(true);
        let paths: Vec<_> = (0..1000)
            .map(|n| PathBuf::from(format!("{n}.png")))
            .collect();
        c.set_scope(&paths, Some(&paths[0]));
        for p in paths.iter().take(MAX_QUEUE) {
            c.queue(p.clone(), QueueCause::SourceOnly, false);
        }
        assert_eq!(c.outstanding.len(), MAX_QUEUE);
        c.prioritize_visible(&paths[900..910]);
        assert!(c.outstanding.iter().all(|p| paths[..MAX_QUEUE].contains(p)));
        c.prioritize_visible(&paths[10..20]);
        let s = c.worker.state.0.lock().unwrap();
        assert_eq!(s.queue[0].path, paths[0]);
        for (index, p) in paths[10..20].iter().enumerate() {
            assert_eq!(s.queue[index + 1].path, *p);
        }
        assert_eq!(s.queue.len(), MAX_QUEUE);
        assert_eq!(c.outstanding.len(), MAX_QUEUE);
    }
    #[test]
    fn empty_scope_refresh_never_requeues_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(None);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        pump(&mut c);
        c.set_scope(&[], None);
        c.refresh();
        pump(&mut c);
        assert!(c.records.is_empty());
    }
    #[test]
    fn pending_import_rejects_component_removal() {
        let mut c = Controller::new(None);
        let (_tx, rx) = mpsc::sync_channel(1);
        c.installation = Some(Installation {
            receiver: rx,
            thread: None,
        });
        assert!(
            c.remove_model()
                .unwrap_err()
                .to_string()
                .contains("组件导入中")
        );
    }
    #[test]
    fn stale_backlog_does_not_drop_new_completion() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..MAX_QUEUE)
            .map(|i| {
                let p = dir.path().join(format!("{i}.png"));
                image(&p);
                p
            })
            .collect();
        let mut c = Controller::new(None);
        c.set_scope(&paths, Some(&paths[0]));
        for p in &paths {
            c.queue(p.clone(), QueueCause::SourceOnly, false);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = c.worker.state.0.lock().unwrap();
            if s.queue.is_empty() && s.active.is_none() {
                break;
            }
            drop(s);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        c.refresh();
        for p in &paths {
            c.queue(p.clone(), QueueCause::SourceOnly, false);
        }
        pump(&mut c);
        assert_eq!(c.records.len(), MAX_QUEUE);
    }
}

#[cfg(test)]
mod v07_controller_tests {
    use super::*;
    fn ready(c: &mut Controller) {
        c.component_state = ComponentState::Ready;
        c.model_pack = Some(PathBuf::from("test-verified-pack"));
    }
    fn pump(c: &mut Controller) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while c.is_busy() && std::time::Instant::now() < until {
            c.poll();
            thread::sleep(std::time::Duration::from_millis(1));
        }
        c.poll();
        assert!(!c.is_busy());
    }
    fn image(path: &Path) {
        image::RgbImage::from_pixel(224, 224, image::Rgb([1, 2, 3]))
            .save(path)
            .unwrap();
    }
    fn verified_component() -> (PathBuf, model::ComponentInfo, InstalledModel) {
        (
            PathBuf::from("test-verified-pack"),
            model::ComponentInfo {
                model_id: model_update::MODEL_ID.into(),
                runtime_version: "fixture".into(),
                installed_bytes: 0,
                license: "fixture",
                source_revision: "fixture".into(),
            },
            serde_json::from_value(serde_json::json!({
                "model_id": model_update::MODEL_ID,
                "release_id": 0,
                "package_sha256": null,
                "profile": model::fingerprint()
            }))
            .unwrap(),
        )
    }
    #[test]
    fn missing_component_manual_detection_stays_unscanned() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(None);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.scan_current(&p);
        pump(&mut c);
        assert!(c.record(&p).is_none() && c.outstanding.is_empty());
        assert_eq!(c.run_state(&p), RunState::Unscanned);
        assert!(c.status().contains("尚未安装"));
    }
    #[test]
    fn installed_component_validation_starts_only_after_manual_detection() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("ai-model/code-linear-fp32");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("manifest.json"), b"invalid fixture manifest").unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(Some(dir.path().join("cache.sqlite")));
        assert!(c.has_installed_model());
        assert!(c.installation.is_none() && !c.is_busy());
        assert!(c.model_status().contains("点击 AI 检测时校验"));
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.set_current(&p);
        c.prioritize_visible(std::slice::from_ref(&p));
        c.poll();
        assert!(c.installation.is_none() && c.records.is_empty());
        assert_eq!(c.decision(&p).label, "尚未检测");

        c.scan_current(&p);
        assert_eq!(c.component_state, ComponentState::Checking);
        assert!(c.installation.is_some());
        assert_eq!(c.pending_manual.as_ref(), Some(&p));
        pump(&mut c);
        assert!(matches!(c.component_state, ComponentState::Failed(_)));
        assert!(c.pending_manual.is_none() && c.record(&p).is_none());
    }
    #[test]
    fn validation_success_continues_the_same_manual_request_exactly_once() {
        let mut c = Controller::new(None);
        let p = PathBuf::from("folder/a.png");
        let other = PathBuf::from("folder/b.png");
        c.set_scope(&[p.clone(), other], Some(&p));
        let (tx, receiver) = mpsc::sync_channel(1);
        c.installation = Some(Installation {
            receiver,
            thread: None,
        });
        c.component_state = ComponentState::Checking;
        c.scan_current(&p);
        c.worker.pause(true);
        assert_eq!(c.pending_manual.as_ref(), Some(&p));
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        assert_eq!(c.pending_manual.as_ref(), Some(&p));
        assert!(c.outstanding.is_empty());
        tx.send(Ok(verified_component())).unwrap();
        c.poll();
        c.poll();
        assert!(c.pending_manual.is_none());
        assert_eq!(c.component_state, ComponentState::Ready);
        assert_eq!(c.outstanding.len(), 1);
        let state = c.worker.state.0.lock().unwrap();
        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue[0].path, p);
        assert_eq!(state.queue[0].cause, QueueCause::UserManual);
        assert!(state.queue[0].model.is_some());
    }
    #[test]
    fn same_current_scope_refresh_preserves_active_manual_task_until_removed() {
        let mut c = Controller::new(None);
        ready(&mut c);
        c.pause(true);
        let p = PathBuf::from("folder/a.png");
        let other = PathBuf::from("folder/b.png");
        c.set_scope(&[p.clone(), other], Some(&p));
        c.queue(p.clone(), QueueCause::UserManual, true);
        let token = c.tokens[&p].1.clone();
        {
            let mut state = c.worker.state.0.lock().unwrap();
            let request = state.queue.pop_front().unwrap();
            state.active = Some((request.path, request.ticket, request.cancel, true));
        }
        let generation = c.generation;
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        assert_eq!(c.generation, generation);
        assert!(token.check().is_ok() && c.outstanding.contains(&p));
        c.set_scope(&[], Some(&p));
        assert!(token.check().is_err() && c.outstanding.is_empty());
    }
    #[test]
    fn navigating_away_cancels_manual_detection_waiting_for_validation() {
        let mut c = Controller::new(None);
        let p = PathBuf::from("folder/a.png");
        let other = PathBuf::from("folder/b.png");
        c.set_scope(&[p.clone(), other.clone()], Some(&p));
        let (tx, receiver) = mpsc::sync_channel(1);
        c.installation = Some(Installation {
            receiver,
            thread: None,
        });
        c.component_state = ComponentState::Checking;
        c.scan_current(&p);
        c.set_current(&other);
        tx.send(Ok(verified_component())).unwrap();
        c.poll();
        assert!(c.pending_manual.is_none() && c.outstanding.is_empty());
        assert!(c.worker.state.0.lock().unwrap().queue.is_empty());
        assert_eq!(c.run_state(&p), RunState::Unscanned);
    }
    #[test]
    fn changing_current_cancels_an_already_queued_manual_detection() {
        let mut c = Controller::new(None);
        ready(&mut c);
        c.pause(true);
        let p = PathBuf::from("folder/a.png");
        let other = PathBuf::from("folder/b.png");
        c.set_scope(&[p.clone(), other.clone()], Some(&p));
        c.queue(p.clone(), QueueCause::UserManual, true);
        let token = c.tokens[&p].1.clone();
        c.set_current(&other);
        assert!(token.check().is_err() && c.outstanding.is_empty());
        assert!(c.worker.state.0.lock().unwrap().queue.is_empty());
    }
    #[test]
    fn browsing_never_resumes_legacy_automatic_settings_or_rewrites_them() {
        assert!(!AiSettings::default().auto_scan);
        let dir = tempfile::tempdir().unwrap();
        let old = AiSettings {
            auto_scan: true,
            show_badges: false,
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&old).unwrap();
        let experimental = br#"{"schema_version":1,"enabled":true,"risk_ack_version":1}"#;
        std::fs::write(dir.path().join("ai-settings.json"), &bytes).unwrap();
        std::fs::write(
            dir.path().join("ai-experimental-settings.json"),
            experimental,
        )
        .unwrap();
        let mut c = Controller::new(Some(dir.path().join("cache.sqlite")));
        assert!(!c.experimental_auto_scan());
        assert!(c.set_experimental_auto_scan(true, true).is_err());
        ready(&mut c);
        assert!(c.set_experimental_auto_scan(true, false).is_err());
        assert!(!c.experimental_auto_scan());
        let paths = [dir.path().join("a.png"), dir.path().join("b.png")];
        c.set_scope(&paths, Some(&paths[0]));
        c.set_current(&paths[1]);
        c.prioritize_visible(&paths);
        c.refresh();
        c.pause(false);
        c.poll();
        assert!(c.records.is_empty() && c.outstanding.is_empty() && !c.is_busy());
        assert!(c.worker.state.0.lock().unwrap().queue.is_empty());
        assert_eq!(
            std::fs::read(dir.path().join("ai-settings.json")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(dir.path().join("ai-experimental-settings.json")).unwrap(),
            experimental
        );
        assert!(c.settings().auto_scan);
        assert!(!c.settings().show_badges);
        assert!(!model::stable_approved());
    }
    #[test]
    fn only_explicit_current_image_request_can_use_pixel_model() {
        let mut c = Controller::new(None);
        ready(&mut c);
        c.pause(true);
        let p = PathBuf::from("a.png");
        c.paths = vec![p.clone()];
        c.scope.insert(p.clone());
        for cause in [
            QueueCause::SourceOnly,
            QueueCause::ExperimentalFolder,
            QueueCause::StableAutomatic,
        ] {
            c.queue(p.clone(), cause, true);
            assert!(
                c.worker
                    .state
                    .0
                    .lock()
                    .unwrap()
                    .queue
                    .front()
                    .unwrap()
                    .model
                    .is_none()
            );
            c.cancel();
        }
        c.queue(p.clone(), QueueCause::UserManual, true);
        assert!(
            c.worker
                .state
                .0
                .lock()
                .unwrap()
                .queue
                .front()
                .unwrap()
                .model
                .is_some()
        );
        c.cancel();
        c.experimental = ExperimentalSettings {
            schema_version: 1,
            enabled: true,
            risk_ack_version: 1,
        };
        c.queue(p.clone(), QueueCause::ExperimentalFolder, true);
        assert!(
            c.worker
                .state
                .0
                .lock()
                .unwrap()
                .queue
                .front()
                .unwrap()
                .model
                .is_none()
        );
    }
    #[test]
    fn network_and_recursive_browsing_never_queue_checks() {
        let mut c = Controller::new(None);
        ready(&mut c);
        c.pause(true);
        let p = PathBuf::from("//server/share/a.png");
        c.set_scope(
            &[p.clone(), PathBuf::from("//server/share/b.png")],
            Some(&p),
        );
        let state = c.worker.state.0.lock().unwrap();
        assert!(state.queue.is_empty());
        drop(state);
        let local = PathBuf::from("folder/a.png");
        c.set_scope(
            &[
                local.clone(),
                PathBuf::from("folder/sub/b.png"),
                PathBuf::from("other/c.png"),
            ],
            Some(&local),
        );
        assert_eq!(c.paths, vec![local]);
    }
    #[test]
    fn clear_automatic_cancels_pending_and_cannot_reappear_on_poll() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(None);
        c.pause(true);
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.queue(p.clone(), QueueCause::SourceOnly, true);
        assert_eq!(c.outstanding.len(), 1);
        c.clear_automatic().unwrap();
        c.pause(false);
        pump(&mut c);
        assert!(c.records.is_empty());
        assert!(c.outstanding.is_empty());
        c.refresh();
        pump(&mut c);
        assert!(c.record(&p).is_none());
        c.inspect_source_current(&p);
        pump(&mut c);
        assert!(c.record(&p).is_some());
    }
    #[test]
    fn old_model_fingerprint_expires_without_erasing_persistent_record_or_manual() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(Some(dir.path().join("cache.sqlite")));
        ready(&mut c);
        let (id, _) = snapshot(&p, &Cancellation::default()).unwrap();
        let mut old = MachineRecord::unscanned(id.clone());
        old.phase = Phase::Complete;
        old.model = Some(ModelResult {
            fingerprint: format!("{}:{}:ort1283", model::MODEL_HASH, model::PREPROCESS),
            input_scope: "center224".into(),
            raw_score: -4.0221,
            threshold: 0.0,
            approved: false,
            high_signal: false,
        });
        c.cache.put_machine(&old).unwrap();
        c.cache
            .set_manual(
                &id.key,
                &ManualOverride {
                    content_hash: id.content_hash.clone(),
                    decision: ManualDecision::Ai,
                    created_unix_seconds: 0,
                },
            )
            .unwrap();
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.inspect_source_current(&p);
        pump(&mut c);
        assert_eq!(c.run_state(&p), RunState::Expired);
        assert!(c.record(&p).unwrap().model.is_none());
        assert!(c.decision(&p).manual_active);
        assert_eq!(
            c.cache
                .get_machine(&id)
                .unwrap()
                .unwrap()
                .model
                .unwrap()
                .raw_score,
            -4.0221
        );
    }
    #[test]
    fn package_revision_invalidates_automatic_result_but_preserves_manual_and_cache() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        image(&p);
        let mut c = Controller::new(Some(dir.path().join("cache.sqlite")));
        ready(&mut c);
        let (id, _) = snapshot(&p, &Cancellation::default()).unwrap();
        let mut old = MachineRecord::unscanned(id.clone());
        old.phase = Phase::Complete;
        old.component_state = ComponentState::Ready;
        old.run_state = Some(RunState::CompletedExperimental);
        old.release_tier = ReleaseTier::Experimental;
        old.pack_revision = Some(format!("9:{}", "a".repeat(64)));
        old.model = Some(ModelResult {
            fingerprint: model::fingerprint(),
            input_scope: "center224".into(),
            raw_score: 1.0,
            threshold: 0.0,
            approved: false,
            high_signal: false,
        });
        c.cache.put_machine(&old).unwrap();
        c.cache
            .set_manual(
                &id.key,
                &ManualOverride {
                    content_hash: id.content_hash.clone(),
                    decision: ManualDecision::NotAi,
                    created_unix_seconds: 0,
                },
            )
            .unwrap();
        c.set_scope(std::slice::from_ref(&p), Some(&p));
        c.inspect_source_current(&p);
        pump(&mut c);
        assert_eq!(c.run_state(&p), RunState::Expired);
        assert!(c.record(&p).unwrap().model.is_none());
        assert!(c.decision(&p).manual_active);
        let retained = c.cache.get_machine(&id).unwrap().unwrap();
        assert_eq!(retained.pack_revision, old.pack_revision);
        assert_eq!(retained.model, old.model);
        assert!(!c.experimental_auto_scan());
    }
    #[test]
    fn corrupt_future_experimental_settings_preserved_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-experimental-settings.json");
        let bytes = br#"{"schema_version":2,"enabled":true,"risk_ack_version":1}"#;
        std::fs::write(&path, bytes).unwrap();
        let c = Controller::new(Some(dir.path().join("cache.sqlite")));
        assert!(!c.experimental_auto_scan());
        assert!(!c.experimental_writable);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
