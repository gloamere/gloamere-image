//! Bounded directory/thumbnail work, independent of full-image loading.
use crate::{Cancellation, Result, ToolError, pipeline};
use std::{
    cmp::Ordering,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;
pub const MAX_DIRECTORY_IMAGES: usize = 8_000;
pub const THUMBNAIL_EDGE: u32 = 96;
pub const THUMBNAIL_CACHE_ENTRIES: usize = 48;

#[derive(Debug)]
pub struct DirectoryListing {
    pub paths: Vec<PathBuf>,
    pub metadata: HashMap<PathBuf, FileMetadata>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FileMetadata {
    pub modified: Option<SystemTime>,
    pub bytes: Option<u64>,
    pub regular_file: bool,
    pub symlink: bool,
}
impl FileMetadata {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            bytes: metadata.is_file().then_some(metadata.len()),
            regular_file: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Natural,
    ModifiedNewest,
    ModifiedOldest,
    SizeLargest,
    SizeSmallest,
}
impl SortOrder {
    pub const ALL: [Self; 5] = [
        Self::Natural,
        Self::ModifiedNewest,
        Self::ModifiedOldest,
        Self::SizeLargest,
        Self::SizeSmallest,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "文件名自然排序",
            Self::ModifiedNewest => "修改时间 · 新到旧",
            Self::ModifiedOldest => "修改时间 · 旧到新",
            Self::SizeLargest => "文件大小 · 大到小",
            Self::SizeSmallest => "文件大小 · 小到大",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormatFilter {
    #[default]
    All,
    Jpeg,
    Png,
    Gif,
    WebP,
    Bmp,
    Ico,
}
impl FormatFilter {
    pub const ALL: [Self; 7] = [
        Self::All,
        Self::Jpeg,
        Self::Png,
        Self::Gif,
        Self::WebP,
        Self::Bmp,
        Self::Ico,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部格式",
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Gif => "GIF",
            Self::WebP => "WebP",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
        }
    }
    pub fn matches(self, path: &Path) -> bool {
        if self == Self::All {
            return true;
        }
        let extension = path.extension().unwrap_or_default().to_string_lossy();
        match self {
            Self::All => true,
            Self::Jpeg => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
            Self::Png => extension.eq_ignore_ascii_case("png"),
            Self::Gif => extension.eq_ignore_ascii_case("gif"),
            Self::WebP => extension.eq_ignore_ascii_case("webp"),
            Self::Bmp => extension.eq_ignore_ascii_case("bmp"),
            Self::Ico => extension.eq_ignore_ascii_case("ico"),
        }
    }
}

fn optional_cmp<T: Ord>(left: Option<T>, right: Option<T>, descending: bool) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) if descending => right.cmp(&left),
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub fn sort_indices(
    paths: &[PathBuf],
    metadata: &HashMap<PathBuf, FileMetadata>,
    indices: &mut [usize],
    order: SortOrder,
) {
    let names: Vec<_> = paths
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
        })
        .collect();
    indices.sort_by(|&left, &right| {
        let a = metadata.get(&paths[left]);
        let b = metadata.get(&paths[right]);
        let by_metadata = match order {
            SortOrder::Natural => Ordering::Equal,
            SortOrder::ModifiedNewest | SortOrder::ModifiedOldest => optional_cmp(
                a.and_then(|m| m.modified),
                b.and_then(|m| m.modified),
                order == SortOrder::ModifiedNewest,
            ),
            SortOrder::SizeLargest | SortOrder::SizeSmallest => optional_cmp(
                a.and_then(|m| m.bytes),
                b.and_then(|m| m.bytes),
                order == SortOrder::SizeLargest,
            ),
        };
        by_metadata
            .then_with(|| natural_cmp(&names[left], &names[right]))
            .then_with(|| paths[left].cmp(&paths[right]))
    });
}
pub fn ensure_current(mut paths: Vec<PathBuf>, current: &Path) -> Result<Vec<PathBuf>> {
    if current.as_os_str().len() > pipeline::MAX_SIBLING_PATH_BYTES {
        return Err(ToolError::Invalid("当前浏览路径超过列表大小限制".into()));
    }
    paths.retain(|path| {
        path.file_name()
            .zip(current.file_name())
            .is_none_or(|(a, b)| {
                a != b
                    && !(cfg!(windows)
                        && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase())
            })
    });
    let mut bytes = paths.iter().map(|p| p.as_os_str().len()).sum::<usize>();
    while paths.len() >= MAX_DIRECTORY_IMAGES
        || bytes.saturating_add(current.as_os_str().len()) > pipeline::MAX_SIBLING_PATH_BYTES
    {
        if let Some(path) = paths.pop() {
            bytes -= path.as_os_str().len();
        } else {
            break;
        }
    }
    paths.push(current.to_path_buf());
    sort_paths(&mut paths);
    Ok(paths)
}
/// Natural order is locale-independent: lowercased Unicode text, ASCII digit
/// magnitude. Naturally equal names use shorter zero padding, then raw paths.
/// Comparing numeric slices rather than parsing integers handles arbitrary runs.
fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let (mut a, mut b) = (0, 0);
    let mut padding_order = Ordering::Equal;
    while a < left.len() && b < right.len() {
        if left[a].is_ascii_digit() && right[b].is_ascii_digit() {
            let (start_a, start_b) = (a, b);
            while a < left.len() && left[a].is_ascii_digit() {
                a += 1;
            }
            while b < right.len() && right[b].is_ascii_digit() {
                b += 1;
            }
            let mut significant_a = start_a;
            let mut significant_b = start_b;
            while significant_a < a && left[significant_a] == b'0' {
                significant_a += 1;
            }
            while significant_b < b && right[significant_b] == b'0' {
                significant_b += 1;
            }
            let order = (a - significant_a)
                .cmp(&(b - significant_b))
                .then_with(|| left[significant_a..a].cmp(&right[significant_b..b]));
            if order != Ordering::Equal {
                return order;
            }
            if padding_order == Ordering::Equal {
                padding_order = (a - start_a).cmp(&(b - start_b));
            }
        } else {
            let order = left[a].cmp(&right[b]);
            if order != Ordering::Equal {
                return order;
            }
            a += 1;
            b += 1;
        }
    }
    (left.len() - a).cmp(&(right.len() - b)).then(padding_order)
}

fn sort_paths(paths: &mut Vec<PathBuf>) {
    // Lowercase each name once; no filesystem order or locale is used to break ties.
    let mut keyed: Vec<_> = paths
        .drain(..)
        .map(|path| {
            let folded = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            (folded, path)
        })
        .collect();
    keyed.sort_unstable_by(|a, b| natural_cmp(&a.0, &b.0).then_with(|| a.1.cmp(&b.1)));
    paths.extend(keyed.into_iter().map(|(_, path)| path));
}

pub fn thumbnail_retryable(error: &ToolError) -> bool {
    match error {
        ToolError::ResourceBusy(_) => true,
        ToolError::Io(error) => {
            matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
            ) || matches!(error.raw_os_error(), Some(32 | 33))
        }
        _ => false,
    }
}
pub fn supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "ico"
        )
    })
}
pub fn directory(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}
pub fn scan(path: &Path, cancel: &Cancellation) -> Result<DirectoryListing> {
    cancel.check()?;
    if path.as_os_str().len() > pipeline::MAX_SIBLING_PATH_BYTES {
        return Err(ToolError::Invalid("当前浏览路径超过列表大小限制".into()));
    }
    let mut paths = vec![path.to_path_buf()];
    let mut metadata = HashMap::new();
    if let Ok(value) = std::fs::symlink_metadata(path) {
        metadata.insert(path.to_path_buf(), FileMetadata::from_metadata(&value));
    }
    let mut bytes = path.as_os_str().len();
    let mut entries = std::fs::read_dir(directory(path))?;
    let mut truncated = false;
    for index in 0..=MAX_DIRECTORY_ENTRIES {
        cancel.check()?;
        let Some(entry) = entries.next() else {
            break;
        };
        if index == MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let candidate = entry.path();
        if !supported(&candidate)
            || candidate
                .file_name()
                .zip(path.file_name())
                .is_some_and(|(a, b)| {
                    a == b
                        || (cfg!(windows)
                            && a.to_string_lossy().to_lowercase()
                                == b.to_string_lossy().to_lowercase())
                })
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let next_bytes = bytes.saturating_add(candidate.as_os_str().len());
        if paths.len() >= MAX_DIRECTORY_IMAGES || next_bytes > pipeline::MAX_SIBLING_PATH_BYTES {
            truncated = true;
            break;
        }
        bytes = next_bytes;
        let value = entry
            .metadata()
            .map(|m| FileMetadata::from_metadata(&m))
            .unwrap_or(FileMetadata {
                regular_file: true,
                ..Default::default()
            });
        metadata.insert(candidate.clone(), value);
        paths.push(candidate);
    }
    cancel.check()?;
    sort_paths(&mut paths);
    paths.dedup();
    Ok(DirectoryListing {
        paths,
        metadata,
        truncated,
    })
}
pub fn thumbnail(path: &Path, cancel: &Cancellation) -> Result<image::RgbaImage> {
    pipeline::thumbnail(path, THUMBNAIL_EDGE, cancel)
}
pub struct Request {
    pub generation: u64,
    pub path: PathBuf,
    pub cancel: Cancellation,
}
pub struct Response<T> {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<T>,
}
type RequestOperation<T> = Box<dyn FnOnce(&Path, &Cancellation) -> Result<T> + Send + 'static>;
struct QueuedRequest<T> {
    request: Request,
    operation: Option<RequestOperation<T>>,
}
struct State<T> {
    pending: Option<QueuedRequest<T>>,
    active: Option<Cancellation>,
    shutdown: bool,
}
impl<T> Default for State<T> {
    fn default() -> Self {
        Self {
            pending: None,
            active: None,
            shutdown: false,
        }
    }
}

/// Measured cooperative shutdown, not a deadline or an interruption guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    pub elapsed: Duration,
    pub cancelled_active: bool,
    pub discarded_pending: bool,
    pub discarded_result: bool,
    pub join_panicked: bool,
}

type ResultSlot<T> = Arc<Mutex<Option<(Cancellation, Response<T>)>>>;

pub struct Worker<T> {
    shared: Arc<(Mutex<State<T>>, Condvar)>,
    output: ResultSlot<T>,
    thread: Option<JoinHandle<()>>,
    name: String,
    shutdown_report: Option<ShutdownReport>,
}
impl<T: Send + 'static> Worker<T> {
    pub fn new(
        name: &str,
        _ctx: eframe::egui::Context,
        operation: impl FnMut(&Path, &Cancellation) -> Result<T> + Send + 'static,
    ) -> Self {
        // Retain the constructor API but never move a Context into this worker.
        // The UI polls while busy. A repaint call can acquire egui's Context
        // lock and deadlock a UI-thread join during Context::run/view teardown.
        Self::start(name, operation)
    }

    fn start(
        name: &str,
        mut operation: impl FnMut(&Path, &Cancellation) -> Result<T> + Send + 'static,
    ) -> Self {
        let shared = Arc::new((Mutex::new(State::<T>::default()), Condvar::new()));
        let output = Arc::new(Mutex::new(None));
        let input = shared.clone();
        let result = output.clone();
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                loop {
                    let QueuedRequest {
                        request,
                        operation: request_operation,
                    } = {
                        let (lock, wake) = &*input;
                        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
                        while state.pending.is_none() && !state.shutdown {
                            state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                        }
                        if state.shutdown {
                            break;
                        }
                        let request = state.pending.take().unwrap();
                        state.active = Some(request.request.cancel.clone());
                        request
                    };
                    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        request.cancel.check()?;
                        let value = match request_operation {
                            Some(operation) => operation(&request.path, &request.cancel),
                            None => operation(&request.path, &request.cancel),
                        }?;
                        request.cancel.check()?;
                        Ok(value)
                    }))
                    .unwrap_or_else(|_| {
                        Err(ToolError::Invalid("图片后台任务异常，原文件未修改".into()))
                    });
                    // Serialize publication with replacement, cancel and shutdown.
                    // All code taking both locks uses state -> output order.
                    let mut state = input.0.lock().unwrap_or_else(|error| error.into_inner());
                    state.active = None;
                    if !state.shutdown && request.cancel.check().is_ok() {
                        *result.lock().unwrap_or_else(|error| error.into_inner()) = Some((
                            request.cancel,
                            Response {
                                generation: request.generation,
                                path: request.path,
                                result: value,
                            },
                        ));
                    }
                }
            })
            .expect("bounded browser worker");
        Self {
            shared,
            output,
            thread: Some(thread),
            name: name.into(),
            shutdown_report: None,
        }
    }
}
impl<T> Worker<T> {
    pub fn request(&self, request: Request) {
        self.enqueue(QueuedRequest {
            request,
            operation: None,
        });
    }
    /// Run one request-specific operation on the existing worker thread. Its
    /// captured data is owned by this request and discarded on replacement.
    pub fn request_with_operation(
        &self,
        request: Request,
        operation: impl FnOnce(&Path, &Cancellation) -> Result<T> + Send + 'static,
    ) {
        self.enqueue(QueuedRequest {
            request,
            operation: Some(Box::new(operation)),
        });
    }
    fn enqueue(&self, queued: QueuedRequest<T>) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
        if state.shutdown {
            queued.request.cancel.cancel();
            return;
        }
        if let Some(active) = &state.active {
            active.cancel();
        }
        if let Some(old) = state.pending.replace(queued) {
            old.request.cancel.cancel();
        }
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(state);
        wake.notify_one();
    }
    pub fn take_result(&self) -> Option<Response<T>> {
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .and_then(|(cancel, response)| cancel.check().is_ok().then_some(response))
    }
    pub fn is_busy(&self) -> bool {
        let state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Include unread results so a completion between take_result() and this
        // check still schedules one more UI poll and cannot get stranded.
        state.active.is_some()
            || state.pending.is_some()
            || self
                .output
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
    }
    pub fn cancel(&self) {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = &state.active {
            active.cancel();
        }
        if let Some(pending) = state.pending.take() {
            pending.request.cancel.cancel();
        }
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
    /// Stop admission, cancel active/pending work, clear results and join once.
    /// No lock is held across join. Codec/OS calls must return before this can
    /// finish; a blocked filesystem call is not forcibly interruptible.
    pub fn shutdown(&mut self) -> ShutdownReport {
        if let Some(report) = self.shutdown_report {
            return report;
        }
        let started = Instant::now();
        log::info!("worker shutdown begin: {}", self.name);
        let (cancelled_active, discarded_pending, discarded_result) = {
            let mut state = self
                .shared
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.shutdown = true;
            let active = state.active.is_some();
            if let Some(cancel) = &state.active {
                cancel.cancel();
            }
            let pending = state.pending.take();
            if let Some(request) = &pending {
                request.request.cancel.cancel();
            }
            let discarded = self
                .output
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .is_some();
            (active, pending.is_some(), discarded)
        };
        self.shared.1.notify_one();
        let join_panicked = self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err());
        let report = ShutdownReport {
            elapsed: started.elapsed(),
            cancelled_active,
            discarded_pending,
            discarded_result,
            join_panicked,
        };
        log::info!("worker shutdown complete: {} {:?}", self.name, report);
        self.shutdown_report = Some(report);
        report
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
        mpsc,
    };

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "worker did not reach expected state"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn request(generation: u64) -> Request {
        Request {
            generation,
            path: PathBuf::from(generation.to_string()),
            cancel: Cancellation::default(),
        }
    }
    fn wait_result<T>(worker: &Worker<T>) -> Response<T> {
        wait_until(|| worker.output.lock().unwrap().is_some());
        worker.take_result().expect("fresh result")
    }

    #[test]
    fn natural_order_handles_numbers_chinese_case_padding_and_prefixes() {
        let expected = [
            "A.png",
            "a.png",
            "image0.png",
            "image00.png",
            "image1.png",
            "image2.png",
            "image02.png",
            "image002.png",
            "image2a.png",
            "image10.png",
            "image99999999999999999999.png",
            "image100000000000000000000.png",
            "图片.png",
            "图片2.GIF",
            "图片2.gif",
            "图片02.gif",
            "图片10.webp",
            "图片集1.png",
        ];
        let mut paths: Vec<_> = expected.iter().rev().map(PathBuf::from).collect();
        sort_paths(&mut paths);
        assert_eq!(
            paths,
            expected.iter().map(PathBuf::from).collect::<Vec<_>>()
        );
        // Different incoming enumeration orders produce the exact same order.
        paths.rotate_left(7);
        sort_paths(&mut paths);
        assert_eq!(
            paths,
            expected.iter().map(PathBuf::from).collect::<Vec<_>>()
        );
        let current = PathBuf::from("image2.png");
        let restored = ensure_current(paths, &current).unwrap();
        assert_eq!(
            restored,
            expected.iter().map(PathBuf::from).collect::<Vec<_>>()
        );
        assert_eq!(natural_cmp("prefix", "prefix2"), Ordering::Less);
        assert_eq!(natural_cmp("0", "00"), Ordering::Less);
        assert_eq!(natural_cmp("2part10", "2part3"), Ordering::Greater);
    }

    #[test]
    fn natural_comparator_is_antisymmetric_and_transitive_on_fixed_samples() {
        let names = [
            "", "0", "00", "a", "a0", "a00", "a0a", "a00a", "a1", "a01", "a2", "a2a", "a02", "a10",
            "a10a", "b", "图2", "图10",
        ];
        for a in names {
            for b in names {
                assert_eq!(natural_cmp(a, b), natural_cmp(b, a).reverse());
                for c in names {
                    if natural_cmp(a, b) != Ordering::Greater
                        && natural_cmp(b, c) != Ordering::Greater
                    {
                        assert_ne!(natural_cmp(a, c), Ordering::Greater, "{a}, {b}, {c}");
                    }
                }
            }
        }
    }

    #[test]
    fn metadata_sort_keeps_missing_values_last_and_natural_names_break_ties() {
        let paths: Vec<_> = ["image10.png", "image2.png", "image1.png", "missing.png"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let mut metadata = HashMap::new();
        for (index, seconds) in [20, 20, 10].into_iter().enumerate() {
            metadata.insert(
                paths[index].clone(),
                FileMetadata {
                    modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
                    bytes: Some(seconds),
                    regular_file: true,
                    symlink: false,
                },
            );
        }
        for order in [SortOrder::ModifiedNewest, SortOrder::SizeLargest] {
            let mut indices = vec![3, 0, 2, 1];
            sort_indices(&paths, &metadata, &mut indices, order);
            assert_eq!(indices, vec![1, 0, 2, 3]);
        }
        for order in [SortOrder::ModifiedOldest, SortOrder::SizeSmallest] {
            let mut indices = vec![3, 0, 2, 1];
            sort_indices(&paths, &metadata, &mut indices, order);
            assert_eq!(indices, vec![2, 1, 0, 3]);
        }
        let mut indices = vec![3, 0, 2, 1];
        sort_indices(&paths, &metadata, &mut indices, SortOrder::Natural);
        assert_eq!(indices, vec![2, 1, 0, 3]);
    }

    #[test]
    fn format_filters_use_case_insensitive_extensions_without_decoding() {
        assert!(FormatFilter::Jpeg.matches(Path::new("IMAGE.JPG")));
        assert!(FormatFilter::Jpeg.matches(Path::new("image.JpEg")));
        assert!(!FormatFilter::Jpeg.matches(Path::new("image.png")));
        assert!(FormatFilter::Png.matches(Path::new("image.PNG")));
        assert!(FormatFilter::Gif.matches(Path::new("image.GIF")));
        assert!(FormatFilter::WebP.matches(Path::new("image.WeBp")));
        assert!(FormatFilter::Bmp.matches(Path::new("image.BmP")));
        assert!(FormatFilter::Ico.matches(Path::new("image.ICO")));
        assert!(!FormatFilter::Bmp.matches(Path::new("image.ico")));
        assert!(!FormatFilter::Ico.matches(Path::new("image.bmp")));
        assert!(FormatFilter::All.matches(Path::new("new-file")));
    }

    #[test]
    fn supported_extensions_include_bitmap_and_icon_without_accepting_containers() {
        for extension in ["jpg", "JPEG", "png", "GIF", "WebP", "BmP", "ICO"] {
            assert!(supported(Path::new(&format!("image.{extension}"))));
        }
        for name in [
            "image",
            "image.cur",
            "image.svg",
            "image.tiff",
            "image.bmp.exe",
        ] {
            assert!(!supported(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn directory_scan_carries_background_file_metadata_for_ui_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("first.png");
        let other = dir.path().join("second.jpg");
        std::fs::write(&source, [0; 5]).unwrap();
        std::fs::write(&other, [0; 13]).unwrap();
        let listing = scan(&source, &Cancellation::default()).unwrap();
        assert_eq!(listing.metadata.len(), 2);
        for (path, bytes) in [(&source, 5), (&other, 13)] {
            let value = &listing.metadata[path];
            assert_eq!(value.bytes, Some(bytes));
            assert_eq!(
                value.modified,
                std::fs::metadata(path).unwrap().modified().ok()
            );
            assert!(value.regular_file && !value.symlink);
        }
    }

    #[test]
    fn worker_replaces_500_requests_and_never_publishes_superseded_work() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = Worker::start("replacement-test", move |path, _cancel| {
            if path == Path::new("1") {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(path.to_path_buf())
        });
        let first = request(1);
        let active = first.cancel.clone();
        worker.request(first);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut previous = Vec::new();
        for generation in 2..=501 {
            let request = request(generation);
            previous.push(request.cancel.clone());
            worker.request(request);
        }
        assert!(active.check().is_err());
        assert!(previous[..499].iter().all(|token| token.check().is_err()));
        assert!(previous[499].check().is_ok());
        assert!(worker.take_result().is_none());
        release_tx.send(()).unwrap();
        let result = wait_result(&worker);
        assert_eq!(result.generation, 501);
        assert_eq!(result.result.unwrap(), Path::new("501"));
        assert!(worker.take_result().is_none());
        assert!(!worker.shutdown().join_panicked);
    }

    #[test]
    fn request_operation_consumes_owned_data_and_preserves_the_default_operation() {
        let mut default_calls = 0;
        let mut worker = Worker::start("request-operation-test", move |path, _| {
            default_calls += 1;
            Ok((
                format!("default-{default_calls}-{}", path.display()),
                thread::current().id(),
            ))
        });
        let owned = String::from("selected-entry");
        worker.request_with_operation(request(1), move |path, cancel| {
            cancel.check()?;
            let entry = owned;
            Ok((
                format!("{entry}-{}", path.display()),
                thread::current().id(),
            ))
        });
        let first = wait_result(&worker).result.unwrap();
        assert_eq!(first.0, "selected-entry-1");
        assert_ne!(first.1, thread::current().id());
        for generation in 2..=3 {
            worker.request(request(generation));
            let result = wait_result(&worker).result.unwrap();
            assert_eq!(result.0, format!("default-{}-{generation}", generation - 1));
            assert_eq!(
                result.1, first.1,
                "all requests reuse the same worker thread"
            );
        }
        assert!(!worker.shutdown().join_panicked);
    }

    #[test]
    fn replaced_request_operations_are_cancelled_and_their_captures_are_reclaimed() {
        struct OwnedCapture(Arc<AtomicUsize>);
        impl Drop for OwnedCapture {
            fn drop(&mut self) {
                self.0.fetch_add(1, AtomicOrdering::AcqRel);
            }
        }

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = Worker::start("request-operation-replacement-test", |_, _| Ok(0_u64));
        let first = request(1);
        let active_cancel = first.cancel.clone();
        worker.request_with_operation(first, move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(1)
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let dropped = Arc::new(AtomicUsize::new(0));
        let executed = Arc::new(AtomicUsize::new(0));
        let mut tokens = Vec::new();
        for generation in 2..=501 {
            let queued = request(generation);
            tokens.push(queued.cancel.clone());
            let owned = OwnedCapture(dropped.clone());
            let executed = executed.clone();
            worker.request_with_operation(queued, move |_, _| {
                let _owned = owned;
                executed.fetch_add(1, AtomicOrdering::AcqRel);
                Ok(generation)
            });
        }
        assert!(active_cancel.check().is_err());
        assert!(tokens[..499].iter().all(|token| token.check().is_err()));
        assert!(tokens[499].check().is_ok());
        assert_eq!(dropped.load(AtomicOrdering::Acquire), 499);
        release_tx.send(()).unwrap();
        let result = wait_result(&worker);
        assert_eq!(result.generation, 501);
        assert_eq!(result.result.unwrap(), 501);
        assert_eq!(executed.load(AtomicOrdering::Acquire), 1);
        assert_eq!(dropped.load(AtomicOrdering::Acquire), 500);
        assert!(!worker.shutdown().join_panicked);

        let rejected = request(502);
        let cancelled = rejected.cancel.clone();
        let owned = OwnedCapture(dropped.clone());
        worker.request_with_operation(rejected, move |_, _| {
            let _owned = owned;
            panic!("a shut-down worker cannot execute an override");
        });
        assert!(cancelled.check().is_err());
        assert_eq!(dropped.load(AtomicOrdering::Acquire), 501);
        assert!(!worker.is_busy());
    }

    #[test]
    fn cancel_clears_completed_slots_and_external_cancel_cannot_be_consumed() {
        let mut worker = Worker::start("cancel-test", |_, _| Ok(()));
        worker.request(request(1));
        wait_until(|| worker.output.lock().unwrap().is_some());
        assert!(worker.is_busy(), "unread result must retain UI polling");
        worker.cancel();
        assert!(!worker.is_busy());
        assert!(worker.take_result().is_none());
        let next = request(2);
        let cancel = next.cancel.clone();
        worker.request(next);
        wait_until(|| worker.output.lock().unwrap().is_some());
        cancel.cancel();
        assert!(worker.take_result().is_none());
        assert!(!worker.shutdown().join_panicked);
    }

    #[test]
    fn cancel_drops_pending_and_prevents_late_active_publication() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = Worker::start("cancel-active-test", move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        });
        let active = request(1);
        let active_cancel = active.cancel.clone();
        worker.request(active);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let pending = request(2);
        let pending_cancel = pending.cancel.clone();
        worker.request(pending);
        worker.cancel();
        assert!(active_cancel.check().is_err());
        assert!(pending_cancel.check().is_err());
        release_tx.send(()).unwrap();
        wait_until(|| !worker.is_busy());
        assert!(worker.take_result().is_none());
    }

    #[test]
    fn shutdown_is_joined_idempotent_and_rejects_new_work() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let finished = Arc::new(AtomicBool::new(false));
        let operation_finished = finished.clone();
        let mut worker = Worker::start("shutdown-test", move |_, cancel| {
            entered_tx.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while cancel.check().is_ok() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            operation_finished.store(true, AtomicOrdering::Release);
            Ok(())
        });
        worker.request(request(1));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let report = worker.shutdown();
        assert!(finished.load(AtomicOrdering::Acquire));
        assert!(report.cancelled_active);
        assert!(!report.join_panicked);
        assert_eq!(worker.shutdown(), report);
        assert!(worker.thread.is_none());
        assert!(!worker.is_busy());
        assert!(worker.take_result().is_none());
        let rejected = request(2);
        let token = rejected.cancel.clone();
        worker.request(rejected);
        assert!(token.check().is_err());
        assert!(!worker.is_busy());
    }

    #[test]
    fn shutdown_clears_pending_and_result_slots() {
        let mut idle = Worker::start("shutdown-output-test", |_, _| Ok(()));
        idle.request(request(1));
        wait_until(|| idle.output.lock().unwrap().is_some());
        assert!(idle.shutdown().discarded_result);
        assert!(idle.take_result().is_none());
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut active = Worker::start("shutdown-pending-test", move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        });
        active.request(request(1));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let pending = request(2);
        let token = pending.cancel.clone();
        active.request(pending);
        let releaser = thread::spawn(move || {
            wait_until(|| token.check().is_err());
            release_tx.send(()).unwrap();
        });
        let report = active.shutdown();
        releaser.join().unwrap();
        assert!(report.cancelled_active);
        assert!(report.discarded_pending);
        assert!(!report.join_panicked);
        assert!(active.take_result().is_none());
    }

    #[test]
    fn context_write_lock_does_not_block_worker_join() {
        let ctx = eframe::egui::Context::default();
        let mut worker = Worker::new("context-lock-test", ctx.clone(), |_, _| {
            thread::sleep(Duration::from_millis(30));
            Ok(())
        });
        worker.request(request(1));
        wait_until(|| worker.shared.0.lock().unwrap().active.is_some());
        ctx.input_mut(|_| {
            assert!(!worker.shutdown().join_panicked);
        });
    }

    #[test]
    fn drop_waits_for_active_owner_and_operation_panic_is_recoverable() {
        let mut worker = Worker::start("panic-test", |path, _| {
            assert_ne!(path, Path::new("1"), "injected operation failure");
            Ok(())
        });
        worker.request(request(1));
        assert!(matches!(
            wait_result(&worker).result,
            Err(ToolError::Invalid(_))
        ));
        worker.request(request(2));
        assert!(wait_result(&worker).result.is_ok());
        assert!(!worker.shutdown().join_panicked);

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let finished = Arc::new(AtomicBool::new(false));
        let operation_finished = finished.clone();
        let worker = Worker::start("drop-test", move |_, _| {
            entered_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(30));
            operation_finished.store(true, AtomicOrdering::Release);
            Ok(())
        });
        worker.request(request(1));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(worker);
        assert!(finished.load(AtomicOrdering::Acquire));
    }

    #[test]
    fn current_outside_capped_listing_is_retained_within_both_budgets() {
        let paths = (0..MAX_DIRECTORY_IMAGES)
            .map(|i| PathBuf::from(format!("{i:05}.png")))
            .collect();
        let current = Path::new("outside.png");
        let paths = ensure_current(paths, current).unwrap();
        assert_eq!(paths.len(), MAX_DIRECTORY_IMAGES);
        assert!(paths.iter().any(|p| p == current));
        let long = vec![
            PathBuf::from("a".repeat(pipeline::MAX_SIBLING_PATH_BYTES / 2)),
            PathBuf::from("b".repeat(pipeline::MAX_SIBLING_PATH_BYTES / 2)),
        ];
        let paths = ensure_current(long, current).unwrap();
        assert!(paths.iter().any(|p| p == current));
        assert!(
            paths.iter().map(|p| p.as_os_str().len()).sum::<usize>()
                <= pipeline::MAX_SIBLING_PATH_BYTES
        );
    }
    #[test]
    fn temporary_budget_and_io_busy_are_retryable_but_damage_is_not() {
        let budget = crate::memory::Budget::new(100);
        let held = budget.reserve(90).unwrap();
        let error = budget.reserve(20).unwrap_err();
        assert!(thumbnail_retryable(&error));
        assert!(!thumbnail_retryable(&budget.reserve(101).unwrap_err()));
        drop(held);
        assert!(budget.reserve(20).is_ok());
        assert!(thumbnail_retryable(
            &std::io::Error::from(std::io::ErrorKind::WouldBlock).into()
        ));
        assert!(!thumbnail_retryable(&ToolError::Invalid(
            "损坏的图片".into()
        )));
    }
    #[test]
    fn directory_filters_orders_and_cancels() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "z.PNG",
            "a.jpeg",
            "中 文.jpg",
            "image10.WeBp",
            "image2.GIF",
            "bitmap.BmP",
            "icon.ICO",
            "ignore.cur",
            "ignore.txt",
        ] {
            std::fs::write(dir.path().join(name), b"fixture").unwrap();
        }
        std::fs::create_dir(dir.path().join("folder.png")).unwrap();
        let source = dir.path().join("z.PNG");
        let listing = scan(&source, &Cancellation::default()).unwrap();
        assert_eq!(listing.paths.len(), 7);
        assert_eq!(listing.paths[0].file_name().unwrap(), "a.jpeg");
        assert!(listing.paths.contains(&dir.path().join("bitmap.BmP")));
        assert!(listing.paths.contains(&dir.path().join("icon.ICO")));
        assert!(!listing.truncated);
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(scan(&source, &cancel), Err(ToolError::Cancelled)));
    }
    #[test]
    fn oversized_directory_has_explicit_cap() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("00000.png");
        for i in 0..=MAX_DIRECTORY_IMAGES {
            std::fs::write(dir.path().join(format!("{i:05}.png")), []).unwrap();
        }
        let listing = scan(&source, &Cancellation::default()).unwrap();
        assert_eq!(listing.paths.len(), MAX_DIRECTORY_IMAGES);
        assert!(listing.truncated);
        assert!(listing.paths.contains(&source));
    }
    #[test]
    fn thumbnail_is_small_and_preserves_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha.png");
        image::RgbaImage::from_pixel(256, 128, image::Rgba([255, 31, 17, 0]))
            .save(&path)
            .unwrap();
        let thumb = thumbnail(&path, &Cancellation::default()).unwrap();
        assert!(thumb.width() <= THUMBNAIL_EDGE && thumb.height() <= THUMBNAIL_EDGE);
        assert!(thumb.pixels().all(|p| p[3] == 0));
        assert!(thumbnail(&dir.path().join("missing.png"), &Cancellation::default()).is_err());
    }

    #[test]
    fn bitmap_and_icon_thumbnails_use_the_bounded_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        for (name, color) in [
            ("bitmap.BMP", [21, 43, 65, 255]),
            ("icon.ICO", [21, 43, 65, 128]),
        ] {
            let path = dir.path().join(name);
            image::RgbaImage::from_pixel(128, 64, image::Rgba(color))
                .save(&path)
                .unwrap();
            let thumb = thumbnail(&path, &Cancellation::default()).unwrap();
            assert_eq!(thumb.dimensions(), (THUMBNAIL_EDGE, THUMBNAIL_EDGE / 2));
            assert!(thumb.pixels().all(|pixel| pixel.0 == color));

            let cancel = Cancellation::default();
            cancel.cancel();
            assert!(matches!(
                thumbnail(&path, &cancel),
                Err(ToolError::Cancelled)
            ));
        }
    }
}
