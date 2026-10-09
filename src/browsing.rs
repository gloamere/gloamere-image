//! Bounded directory/thumbnail work, independent of full-image loading.
use crate::{Cancellation, Result, ToolError, pipeline};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};

pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;
pub const MAX_DIRECTORY_IMAGES: usize = 8_000;
pub const THUMBNAIL_EDGE: u32 = 96;
pub const THUMBNAIL_CACHE_ENTRIES: usize = 48;

#[derive(Debug)]
pub struct DirectoryListing {
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
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
    paths.sort_by_cached_key(|p| {
        (
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
            p.clone(),
        )
    });
    Ok(paths)
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
            "jpg" | "jpeg" | "png"
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
    let mut paths = vec![path.to_path_buf()];
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
            || !entry.file_type().is_ok_and(|t| t.is_file())
        {
            continue;
        }
        let next_bytes = bytes.saturating_add(candidate.as_os_str().len());
        if paths.len() >= MAX_DIRECTORY_IMAGES || next_bytes > pipeline::MAX_SIBLING_PATH_BYTES {
            truncated = true;
            break;
        }
        bytes = next_bytes;
        paths.push(candidate);
    }
    cancel.check()?;
    paths.sort_by_cached_key(|p| {
        (
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
            p.clone(),
        )
    });
    paths.dedup();
    Ok(DirectoryListing { paths, truncated })
}
pub fn thumbnail(path: &Path, cancel: &Cancellation) -> Result<image::RgbaImage> {
    let loaded = pipeline::load(path, cancel)?;
    cancel.check()?;
    let thumbnail = image::imageops::thumbnail(&**loaded.pixels, THUMBNAIL_EDGE, THUMBNAIL_EDGE);
    cancel.check()?;
    Ok(thumbnail)
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
#[derive(Default)]
struct State {
    pending: Option<Request>,
    active: Option<Cancellation>,
    shutdown: bool,
}
pub struct Worker<T> {
    shared: Arc<(Mutex<State>, Condvar)>,
    output: Arc<Mutex<Option<Response<T>>>>,
}
impl<T: Send + 'static> Worker<T> {
    pub fn new(
        name: &str,
        ctx: eframe::egui::Context,
        operation: fn(&Path, &Cancellation) -> Result<T>,
    ) -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let output = Arc::new(Mutex::new(None));
        let input = shared.clone();
        let result = output.clone();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                loop {
                    let request = {
                        let (lock, wake) = &*input;
                        let mut state = lock.lock().unwrap();
                        while state.pending.is_none() && !state.shutdown {
                            state = wake.wait(state).unwrap();
                        }
                        if state.shutdown {
                            break;
                        }
                        let request = state.pending.take().unwrap();
                        state.active = Some(request.cancel.clone());
                        request
                    };
                    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let value = operation(&request.path, &request.cancel)?;
                        request.cancel.check()?;
                        Ok(value)
                    }))
                    .unwrap_or_else(|_| Err(ToolError::Invalid("图片列表后台异常".into())));
                    input.0.lock().unwrap().active = None;
                    *result.lock().unwrap() = Some(Response {
                        generation: request.generation,
                        path: request.path,
                        result: value,
                    });
                    ctx.request_repaint();
                }
            })
            .expect("bounded browser worker");
        Self { shared, output }
    }
    pub fn request(&self, request: Request) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        if let Some(active) = &state.active {
            active.cancel();
        }
        if let Some(old) = state.pending.replace(request) {
            old.cancel.cancel();
        }
        drop(state);
        wake.notify_one();
    }
    pub fn take_result(&self) -> Option<Response<T>> {
        self.output.lock().unwrap().take()
    }
    pub fn cancel(&self) {
        let mut state = self.shared.0.lock().unwrap();
        if let Some(active) = &state.active {
            active.cancel();
        }
        if let Some(pending) = state.pending.take() {
            pending.cancel.cancel();
        }
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.shutdown = true;
        if let Some(active) = &state.active {
            active.cancel();
        }
        if let Some(pending) = state.pending.take() {
            pending.cancel.cancel();
        }
        wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        for name in ["z.PNG", "a.jpeg", "中 文.jpg", "ignore.bmp", "ignore.txt"] {
            std::fs::write(dir.path().join(name), b"fixture").unwrap();
        }
        std::fs::create_dir(dir.path().join("folder.png")).unwrap();
        let source = dir.path().join("z.PNG");
        let listing = scan(&source, &Cancellation::default()).unwrap();
        assert_eq!(listing.paths.len(), 3);
        assert_eq!(listing.paths[0].file_name().unwrap(), "a.jpeg");
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
}
