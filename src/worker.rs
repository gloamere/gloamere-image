//! Main-image adapter over the same bounded, owned worker used by browsing.
use crate::{
    Cancellation, Result, ToolError,
    browsing::{Request, Worker},
    pipeline::{self, LoadedImage},
};
use std::{
    collections::VecDeque,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

pub use crate::browsing::ShutdownReport;

const RECENT_IMAGES: usize = 2;
const RECENT_PIXEL_BYTES: u64 = 192 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub retained_pixel_bytes: u64,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    bytes: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    native: NativeStamp,
}

#[derive(PartialEq, Eq)]
struct NativeStamp {
    volume: u64,
    file: u64,
    changed: i128,
}

impl FileStamp {
    fn read(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        Self::from_file(&file)
    }

    fn from_file(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            bytes: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            native: native_stamp(file, &metadata)?,
        })
    }

    fn matches_current(&self, file: &File, path: &Path) -> bool {
        Some(self) == Self::from_file(file).as_ref() && Some(self) == Self::read(path).as_ref()
    }
}

fn native_stamp(_file: &File, _metadata: &std::fs::Metadata) -> Option<NativeStamp> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo,
                GetFileInformationByHandle, GetFileInformationByHandleEx,
            },
        };
        // File IDs reject a same-name replacement; ChangeTime also detects an
        // in-place rewrite that preserves the original last-write timestamp.
        unsafe {
            let handle = _file.as_raw_handle() as HANDLE;
            let mut identity: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let mut basic: FILE_BASIC_INFO = std::mem::zeroed();
            if GetFileInformationByHandle(handle, &mut identity) == 0
                || GetFileInformationByHandleEx(
                    handle,
                    FileBasicInfo,
                    (&mut basic as *mut FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                ) == 0
            {
                return None;
            }
            Some(NativeStamp {
                volume: u64::from(identity.dwVolumeSerialNumber),
                file: (u64::from(identity.nFileIndexHigh) << 32)
                    | u64::from(identity.nFileIndexLow),
                changed: i128::from(basic.ChangeTime),
            })
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(NativeStamp {
            volume: _metadata.dev(),
            file: _metadata.ino(),
            changed: i128::from(_metadata.ctime()) * 1_000_000_000
                + i128::from(_metadata.ctime_nsec()),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

struct RecentImage {
    stamp: FileStamp,
    image: LoadedImage,
    bytes: u64,
}

struct RecentImages {
    entries: VecDeque<RecentImage>,
    limit: u64,
    epoch: u64,
    stats: CacheStats,
}

impl Default for RecentImages {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            limit: RECENT_PIXEL_BYTES,
            epoch: 0,
            stats: CacheStats::default(),
        }
    }
}

impl RecentImages {
    fn remove(&mut self, index: usize) -> Option<RecentImage> {
        let removed = self.entries.remove(index)?;
        self.stats.retained_pixel_bytes -= removed.bytes;
        self.stats.entries = self.entries.len();
        self.stats.evictions += 1;
        Some(removed)
    }

    fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.stats.evictions += self.entries.len() as u64;
        self.entries.clear();
        self.stats.entries = 0;
        self.stats.retained_pixel_bytes = 0;
    }

    fn lookup(&mut self, path: &Path, stamp: Option<&FileStamp>) -> Option<LoadedImage> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.image.path == path)?;
        if stamp != Some(&self.entries[index].stamp) {
            self.remove(index);
            return None;
        }
        let entry = self.entries.remove(index).unwrap();
        let image = entry.image.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, stamp: FileStamp, image: &LoadedImage) {
        // Animation owns compressed snapshots and decoder state as well as
        // pixels. Only immutable static originals enter this small cache.
        let bytes = (image.pixels.as_raw().capacity() as u64).saturating_mul(2);
        if image.animation.is_some() || bytes > self.limit {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.image.path == image.path)
        {
            self.remove(index);
        }
        while self.entries.len() >= RECENT_IMAGES
            || self.stats.retained_pixel_bytes.saturating_add(bytes) > self.limit
        {
            self.remove(0);
        }
        self.entries.push_back(RecentImage {
            stamp,
            image: image.clone(),
            bytes,
        });
        // Count both original RGBA and its prepared display pixels. Arc clones
        // keep their existing reservations instead of duplicating either buffer.
        self.stats.retained_pixel_bytes += bytes;
        self.stats.entries = self.entries.len();
    }
}

type SharedRecentImages = Arc<Mutex<RecentImages>>;

fn load_recent(
    cache: &SharedRecentImages,
    path: &Path,
    cancel: &Cancellation,
    mut operation: impl FnMut(&Path, &mut File, &Cancellation) -> Result<LoadedImage>,
) -> Result<LoadedImage> {
    let started = Instant::now();
    cancel.check()?;
    let mut file = File::open(path)?;
    let mut before = FileStamp::from_file(&file);
    cancel.check()?;
    let cached = {
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        cache.lookup(path, before.as_ref())
    };
    if let Some(mut image) = cached {
        cancel.check()?;
        image
            .pixels
            .prepare_display_with_alpha(cancel, Some(image.has_alpha))?;
        if before
            .as_ref()
            .is_some_and(|stamp| stamp.matches_current(&file, path))
        {
            cancel.check()?;
            cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .stats
                .hits += 1;
            image.load_ms = started.elapsed().as_millis();
            return Ok(image);
        }
        // A path replacement during lookup must not return pixels for the
        // earlier file. Decode a fresh handle instead of trusting the cache.
        cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lookup(path, None);
        file = File::open(path)?;
        before = FileStamp::from_file(&file);
    }
    let mut epoch = {
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        cache.stats.misses += 1;
        cache.epoch
    };
    let mut result = operation(path, &mut file, cancel);
    cancel.check()?;
    if matches!(&result, Err(ToolError::ResourceBusy(_))) {
        let released = {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            let released = !cache.entries.is_empty();
            cache.clear();
            epoch = cache.epoch;
            released
        };
        if released {
            result = operation(path, &mut file, cancel);
        }
    }
    cancel.check()?;
    let image = result?;
    if let Err(error) = image
        .pixels
        .prepare_display_with_alpha(cancel, Some(image.has_alpha))
    {
        if !matches!(&error, ToolError::ResourceBusy(_)) {
            return Err(error);
        }
        let released = {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            let released = !cache.entries.is_empty();
            cache.clear();
            epoch = cache.epoch;
            released
        };
        if !released {
            return Err(error);
        }
        image
            .pixels
            .prepare_display_with_alpha(cancel, Some(image.has_alpha))?;
    }
    // The decoder reads this same handle. A->B->A path swaps cannot bind B's
    // snapshot to A's identity; in-place writes and unresolved paths skip caching.
    if let Some(before) = before.filter(|before| before.matches_current(&file, path)) {
        cancel.check()?;
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        if cache.epoch == epoch {
            cache.insert(before, &image);
        }
    }
    Ok(image)
}

pub struct LoadRequest {
    pub generation: u64,
    pub path: PathBuf,
    pub cancel: Cancellation,
}
pub struct LoadResponse {
    pub generation: u64,
    pub result: Result<LoadedImage>,
    pub siblings: Vec<PathBuf>,
}
pub struct Loader {
    worker: Worker<LoadedImage>,
    cache: SharedRecentImages,
}
impl Loader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let cache = Arc::new(Mutex::new(RecentImages::default()));
        let worker_cache = cache.clone();
        Self {
            worker: Worker::new("image-loader", ctx, move |path, cancel| {
                load_recent(&worker_cache, path, cancel, pipeline::load_open_file)
            }),
            cache,
        }
    }
    pub fn request(&self, request: LoadRequest) {
        self.worker.request(Request {
            generation: request.generation,
            path: request.path,
            cancel: request.cancel,
        });
    }
    pub fn request_icon_entry(&self, request: LoadRequest, index: usize) {
        self.request_variant(request, move |path, cancel| {
            pipeline::load_icon_entry(path, index, cancel)
        });
    }
    pub fn request_icon_reload(&self, request: LoadRequest, dimensions: [u32; 2], bit_depth: u16) {
        self.request_variant(request, move |path, cancel| {
            pipeline::reload_icon(path, dimensions, bit_depth, cancel)
        });
    }
    fn request_variant(
        &self,
        request: LoadRequest,
        operation: impl FnOnce(&Path, &Cancellation) -> Result<LoadedImage> + Send + 'static,
    ) {
        self.worker.request_with_operation(
            Request {
                generation: request.generation,
                path: request.path,
                cancel: request.cancel,
            },
            move |path, cancel| {
                let image = operation(path, cancel)?;
                image
                    .pixels
                    .prepare_display_with_alpha(cancel, Some(image.has_alpha))?;
                cancel.check()?;
                Ok(image)
            },
        );
    }
    pub fn take_result(&self) -> Option<LoadResponse> {
        self.worker.take_result().map(|response| LoadResponse {
            generation: response.generation,
            result: response.result,
            // Directory scanning stays independent of successfully decoded pixels.
            siblings: vec![response.path],
        })
    }
    pub fn cancel(&self) {
        self.worker.cancel();
    }
    pub fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }
    pub fn clear_cache(&self) {
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stats
    }
    pub fn shutdown(&mut self) -> ShutdownReport {
        let report = self.worker.shutdown();
        self.clear_cache();
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;
    use std::time::{Duration, Instant};

    fn fixture(path: &Path, color: [u8; 4]) {
        image::RgbaImage::from_pixel(2, 3, image::Rgba(color))
            .save(path)
            .unwrap();
    }

    fn request_image(loader: &Loader, path: &Path, generation: u64) -> LoadedImage {
        loader.request(LoadRequest {
            generation,
            path: path.to_path_buf(),
            cancel: Cancellation::default(),
        });
        let response = wait_result(loader);
        assert_eq!(response.generation, generation);
        response.result.unwrap()
    }

    fn wait_result(loader: &Loader) -> LoadResponse {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(response) = loader.take_result() {
                return response;
            }
            assert!(Instant::now() < deadline, "loader result deadline");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn loader_recovers_from_bad_file_and_preserves_selected_path_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.png");
        let image = dir.path().join("图片2.png");
        image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]))
            .save(&image)
            .unwrap();
        let mut loader = Loader::new(eframe::egui::Context::default());
        loader.request(LoadRequest {
            generation: 1,
            path: missing.clone(),
            cancel: Cancellation::default(),
        });
        let error = wait_result(&loader);
        assert_eq!(error.generation, 1);
        assert_eq!(error.siblings, vec![missing]);
        assert!(error.result.is_err());
        loader.request(LoadRequest {
            generation: 2,
            path: image.clone(),
            cancel: Cancellation::default(),
        });
        let response = wait_result(&loader);
        assert_eq!(response.generation, 2);
        assert_eq!(response.siblings, vec![image]);
        let loaded = response.result.unwrap();
        assert_eq!(loaded.pixels.dimensions(), (2, 3));
        assert!(!loader.is_busy());
        let report = loader.shutdown();
        assert!(!report.join_panicked);
        assert_eq!(loader.shutdown(), report);
        let token = Cancellation::default();
        loader.request(LoadRequest {
            generation: 3,
            path: dir.path().join("unused.png"),
            cancel: token.clone(),
        });
        assert!(token.check().is_err());
        assert!(loader.take_result().is_none());
    }

    #[test]
    fn loader_reuses_recent_original_pixels_and_invalidates_external_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        fixture(&a, [1, 2, 3, 255]);
        fixture(&b, [4, 5, 6, 255]);
        let loader = Loader::new(eframe::egui::Context::default());
        let first = request_image(&loader, &a, 1);
        request_image(&loader, &b, 2);
        let reused = request_image(&loader, &a, 3);
        assert!(Arc::ptr_eq(&first.pixels, &reused.pixels));
        assert_eq!(loader.cache_stats().hits, 1);
        assert_eq!(loader.cache_stats().entries, 2);
        fixture(&a, [7, 8, 9, 255]);
        let changed = request_image(&loader, &a, 4);
        assert!(!Arc::ptr_eq(&first.pixels, &changed.pixels));
        assert_ne!(first.source_sha256, changed.source_sha256);
        assert_eq!(changed.pixels.get_pixel(0, 0).0, [7, 8, 9, 255]);
        loader.clear_cache();
        assert_eq!(loader.cache_stats().entries, 0);
        assert_eq!(loader.cache_stats().retained_pixel_bytes, 0);
    }

    #[test]
    fn file_stamp_rejects_replacement_with_preserved_size_and_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same-name");
        let replacement = dir.path().join("replacement");
        std::fs::write(&path, b"aaaa").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = FileStamp::read(&path).unwrap();
        std::fs::write(&replacement, b"bbbb").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let after = FileStamp::read(&path).unwrap();
        assert_eq!(before.bytes, after.bytes);
        assert_eq!(before.modified, after.modified);
        assert!(before != after);
    }

    #[test]
    fn hit_revalidation_rejects_a_path_replaced_after_opening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.png");
        let saved = dir.path().join("saved.png");
        let replacement = dir.path().join("replacement.png");
        fixture(&path, [1, 2, 3, 255]);
        fixture(&replacement, [7, 8, 9, 255]);
        let file = File::open(&path).unwrap();
        let before = FileStamp::from_file(&file).unwrap();
        assert!(before.matches_current(&file, &path));
        std::fs::rename(&path, &saved).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(!before.matches_current(&file, &path));
    }

    #[test]
    fn decode_uses_the_stamped_handle_during_an_a_b_a_path_swap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.png");
        let saved = dir.path().join("saved.png");
        let replacement = dir.path().join("replacement.png");
        fixture(&path, [1, 2, 3, 255]);
        fixture(&replacement, [7, 8, 9, 255]);
        let expected = pipeline::load(&path, &Cancellation::default()).unwrap();
        let cache = Arc::new(Mutex::new(RecentImages::default()));
        let loaded = load_recent(
            &cache,
            &path,
            &Cancellation::default(),
            |path, file, cancel| {
                std::fs::rename(path, &saved).unwrap();
                std::fs::rename(&replacement, path).unwrap();
                let image = pipeline::load_open_file(path, file, cancel);
                std::fs::rename(path, &replacement).unwrap();
                std::fs::rename(&saved, path).unwrap();
                image
            },
        )
        .unwrap();
        assert_eq!(loaded.source_sha256, expected.source_sha256);
        assert_eq!(loaded.pixels.get_pixel(0, 0).0, [1, 2, 3, 255]);
        let next = load_recent(
            &cache,
            &path,
            &Cancellation::default(),
            pipeline::load_open_file,
        )
        .unwrap();
        assert_eq!(next.source_sha256, expected.source_sha256);
        assert_eq!(next.pixels.get_pixel(0, 0).0, [1, 2, 3, 255]);
    }

    #[test]
    fn recent_cache_obeys_lru_entry_and_raw_plus_display_byte_limits() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ["a.png", "b.png", "c.png"].map(|name| dir.path().join(name));
        for path in &paths {
            fixture(path, [1, 2, 3, 255]);
        }
        let images = paths
            .iter()
            .map(|path| pipeline::load(path, &Cancellation::default()).unwrap())
            .collect::<Vec<_>>();
        let mut cache = RecentImages::default();
        for image in &images[..2] {
            cache.insert(FileStamp::read(&image.path).unwrap(), image);
        }
        assert!(
            cache
                .lookup(&paths[0], FileStamp::read(&paths[0]).as_ref())
                .is_some()
        );
        cache.insert(FileStamp::read(&paths[2]).unwrap(), &images[2]);
        assert_eq!(cache.entries.len(), RECENT_IMAGES);
        assert!(
            cache
                .lookup(&paths[1], FileStamp::read(&paths[1]).as_ref())
                .is_none()
        );
        assert!(
            cache
                .lookup(&paths[0], FileStamp::read(&paths[0]).as_ref())
                .is_some()
        );
        cache.clear();
        let bytes = images[0].pixels.as_raw().capacity() as u64 * 2;
        cache.limit = bytes;
        for image in &images {
            cache.insert(FileStamp::read(&image.path).unwrap(), image);
        }
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.stats.retained_pixel_bytes, bytes);
        cache.clear();
        cache.limit = bytes - 1;
        cache.insert(FileStamp::read(&paths[0]).unwrap(), &images[0]);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn cache_clear_during_decode_and_cancelled_decode_cannot_refill_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        fixture(&path, [1, 2, 3, 255]);
        let cache = Arc::new(Mutex::new(RecentImages::default()));
        let loaded = load_recent(
            &cache,
            &path,
            &Cancellation::default(),
            |path, file, cancel| {
                let image = pipeline::load_open_file(path, file, cancel)?;
                cache.lock().unwrap().clear();
                Ok(image)
            },
        )
        .unwrap();
        assert_eq!(loaded.pixels.dimensions(), (2, 3));
        assert!(cache.lock().unwrap().entries.is_empty());
        let cancel = Cancellation::default();
        assert!(matches!(
            load_recent(&cache, &path, &cancel, |path, file, cancel| {
                let image = pipeline::load_open_file(path, file, cancel)?;
                cancel.cancel();
                Ok(image)
            }),
            Err(ToolError::Cancelled)
        ));
        assert!(cache.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn decode_budget_pressure_releases_cached_pixels_before_one_retry() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        fixture(&a, [1, 2, 3, 255]);
        fixture(&b, [4, 5, 6, 255]);
        let cache = Arc::new(Mutex::new(RecentImages::default()));
        load_recent(
            &cache,
            &a,
            &Cancellation::default(),
            pipeline::load_open_file,
        )
        .unwrap();
        let mut attempts = 0;
        let result = load_recent(
            &cache,
            &b,
            &Cancellation::default(),
            |path, file, cancel| {
                attempts += 1;
                if attempts == 1 {
                    return Err(ToolError::ResourceBusy("injected cache pressure".into()));
                }
                assert!(cache.lock().unwrap().entries.is_empty());
                pipeline::load_open_file(path, file, cancel)
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(result.path, b);
        assert_eq!(cache.lock().unwrap().entries.len(), 1);
    }

    #[test]
    fn animation_sources_are_not_retained_as_recent_static_originals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("animated.gif");
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&path).unwrap());
            encoder
                .encode_frames([[255, 0, 0, 255], [0, 255, 0, 255]].map(|color| {
                    image::Frame::new(image::RgbaImage::from_pixel(2, 3, image::Rgba(color)))
                }))
                .unwrap();
        }
        let cache = Arc::new(Mutex::new(RecentImages::default()));
        let image = load_recent(
            &cache,
            &path,
            &Cancellation::default(),
            pipeline::load_open_file,
        )
        .unwrap();
        assert!(image.animation.is_some());
        assert!(cache.lock().unwrap().entries.is_empty());
    }
}
