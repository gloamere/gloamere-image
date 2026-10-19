//! Win32 ownership is confined here. No clipboard listener/delayed rendering.
use super::{codec, *};
use std::{
    ptr,
    sync::{Mutex, MutexGuard},
};
use windows_sys::Win32::{
    Foundation::{ERROR_ACCESS_DENIED, GetLastError, GlobalFree, HGLOBAL, HWND, SetLastError},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_REPARSE_POINT,
        GetDriveTypeW, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    },
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT},
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE},
};

static OPERATIONS: Mutex<()> = Mutex::new(());
fn serial() -> ClipboardResult<MutexGuard<'static, ()>> {
    OPERATIONS.try_lock().map_err(|error| match error {
        std::sync::TryLockError::WouldBlock => ClipboardError::Busy,
        std::sync::TryLockError::Poisoned(_) => {
            ClipboardError::Resource("上次剪贴板操作异常终止，请重新启动".into())
        }
    })
}
fn os_error(operation: &'static str) -> ClipboardError {
    // SAFETY: GetLastError has no preconditions; capture before another Win32 call.
    ClipboardError::Platform {
        operation,
        code: unsafe { GetLastError() },
    }
}
fn write_error(operation: &'static str) -> ClipboardError {
    ClipboardError::WriteFailed {
        operation,
        code: unsafe { GetLastError() },
    }
}

struct OpenGuard(bool);
impl OpenGuard {
    fn open(owner: HWND) -> ClipboardResult<Self> {
        // SAFETY: owner is null for reads or our live window for writes.
        if unsafe { OpenClipboard(owner) } == 0 {
            let code = unsafe { GetLastError() };
            return Err(if code == ERROR_ACCESS_DENIED {
                ClipboardError::Busy
            } else {
                ClipboardError::Platform {
                    operation: "OpenClipboard",
                    code,
                }
            });
        }
        Ok(Self(true))
    }
    fn close(mut self, writing: bool) -> ClipboardResult<()> {
        if unsafe { CloseClipboard() } == 0 {
            return Err(if writing {
                write_error("CloseClipboard")
            } else {
                os_error("CloseClipboard")
            });
        }
        self.0 = false;
        Ok(())
    }
}
impl Drop for OpenGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: this guard owns the successful OpenClipboard call.
            unsafe {
                CloseClipboard();
            }
        }
    }
}

/// Reads borrowed OS data only while its clipboard and global-memory locks live.
struct BorrowedMemory<'a> {
    handle: HGLOBAL,
    pointer: *const u8,
    length: usize,
    _open: std::marker::PhantomData<&'a OpenGuard>,
}
impl<'a> BorrowedMemory<'a> {
    fn read(format: u32, limit: usize, _open: &'a OpenGuard) -> ClipboardResult<Self> {
        // SAFETY: the clipboard has been opened and remains owned by _open.
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return Err(os_error("GetClipboardData"));
        }
        // For only the supported PNG/DIB/text/DROPFILES formats the handle is HGLOBAL.
        let length = unsafe { GlobalSize(handle) };
        if length == 0 {
            return Err(os_error("GlobalSize"));
        }
        if length > limit {
            return Err(ClipboardError::TooLarge(
                "系统剪贴板块超过对应格式的长度限制",
            ));
        }
        let pointer = unsafe { GlobalLock(handle) } as *const u8;
        if pointer.is_null() {
            return Err(os_error("GlobalLock"));
        }
        Ok(Self {
            handle,
            pointer,
            length,
            _open: std::marker::PhantomData,
        })
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: pointer refers to the locked HGLOBAL's checked byte length.
        // The borrowed handle is never freed or used outside OpenGuard's lifetime.
        unsafe { std::slice::from_raw_parts(self.pointer, self.length) }
    }
}
impl Drop for BorrowedMemory<'_> {
    fn drop(&mut self) {
        // SAFETY: exactly balances our successful GlobalLock, never frees OS data.
        unsafe {
            GlobalUnlock(self.handle);
        }
    }
}

fn snapshot(bytes: &[u8]) -> ClipboardResult<crate::memory::Snapshot> {
    let reservation = crate::memory::reserve(bytes.len() as u64).map_err(admission)?;
    let mut output = codec::zeroed(bytes.len())?;
    output.copy_from_slice(bytes);
    crate::memory::Snapshot::from_reserved(output, reservation).map_err(admission)
}
fn png_format() -> ClipboardResult<u32> {
    // SAFETY: compile-time, NUL-terminated UTF-16 format name.
    let format = unsafe { RegisterClipboardFormatW(windows_sys::w!("PNG")) };
    if format == 0 {
        Err(os_error("RegisterClipboardFormatW(PNG)"))
    } else {
        Ok(format)
    }
}
fn available(format: u32) -> bool {
    // This queries only; no listener or polling is installed.
    unsafe { IsClipboardFormatAvailable(format) != 0 }
}

enum ReadPayload {
    Png(crate::memory::Snapshot),
    Dib(crate::memory::Snapshot, ClipboardSource),
    File(PathBuf),
}

pub(super) fn read_image_or_file() -> ClipboardResult<ClipboardInput> {
    let _serial = serial()?;
    let png = png_format()?;
    let open = OpenGuard::open(ptr::null_mut())?;
    // Fail closed if the highest-priority advertised pixel format is malformed or
    // over budget, rather than silently substituting unrelated text/file content.
    let payload = if available(png) {
        let memory = BorrowedMemory::read(png, MAX_CLIPBOARD_BYTES, &open)?;
        codec::png_dimensions(memory.bytes())?; // before snapshot allocation
        ReadPayload::Png(snapshot(memory.bytes())?)
    } else if available(CF_DIBV5 as u32) || available(CF_DIB as u32) {
        let (format, source) = if available(CF_DIBV5 as u32) {
            (CF_DIBV5 as u32, ClipboardSource::DibV5)
        } else {
            (CF_DIB as u32, ClipboardSource::Dib)
        };
        let memory = BorrowedMemory::read(format, MAX_CLIPBOARD_BYTES, &open)?;
        codec::dib_layout(memory.bytes(), source)?; // before snapshot allocation
        ReadPayload::Dib(snapshot(memory.bytes())?, source)
    } else if available(CF_HDROP as u32) {
        let memory = BorrowedMemory::read(CF_HDROP as u32, MAX_TEXT_BYTES + 22, &open)?;
        ReadPayload::File(codec::decode_hdrop(memory.bytes())?)
    } else if available(CF_UNICODETEXT as u32) {
        let memory = BorrowedMemory::read(CF_UNICODETEXT as u32, MAX_TEXT_BYTES, &open)?;
        ReadPayload::File(codec::local_path(&codec::decode_unicode_text(
            memory.bytes(),
        )?)?)
    } else {
        return Err(ClipboardError::NoSupportedFormat);
    };
    // Release the system lock before decoding or checking the local filesystem.
    open.close(false)?;
    match payload {
        ReadPayload::Png(bytes) => codec::decode_png(&bytes).map(ClipboardInput::Pixels),
        ReadPayload::Dib(bytes, source) => {
            codec::decode_dib(&bytes, source).map(ClipboardInput::Pixels)
        }
        ReadPayload::File(path) => {
            validate_local_file(&path)?;
            Ok(ClipboardInput::File(path))
        }
    }
}

fn validate_local_file(path: &std::path::Path) -> ClipboardResult<()> {
    use std::os::windows::ffi::OsStrExt;
    // Lexical validation guarantees a drive-absolute path and no dot/device pieces.
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.len() > MAX_PATH_UNITS || wide.len() < 4 {
        return Err(ClipboardError::NonLocalPath);
    }
    let root = [wide[0], b':' as u16, b'\\' as u16, 0];
    // Accept only local removable/fixed/CD-ROM/RAM drives, never mapped networks.
    if !matches!(unsafe { GetDriveTypeW(root.as_ptr()) }, 2 | 3 | 5 | 6) {
        return Err(ClipboardError::NonLocalPath);
    }
    wide.push(0);
    // Inspect ancestors from root downward before touching their children. Reject
    // junctions, symlinks, cloud placeholders and offline data rather than hydrate.
    // This is not an adversarial-filesystem TOCTOU sandbox; normal file loading
    // rechecks regular-file metadata and byte limits in the application pipeline.
    for end in (3..wide.len() - 1)
        .filter(|index| wide[*index] == b'\\' as u16)
        .chain(std::iter::once(wide.len() - 1))
    {
        let mut prefix = wide[..end].to_vec();
        prefix.push(0);
        let attributes = unsafe { GetFileAttributesW(prefix.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(os_error("GetFileAttributesW"));
        }
        if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_OFFLINE) != 0 {
            return Err(ClipboardError::NonLocalPath);
        }
        if end == wide.len() - 1 && attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(ClipboardError::Invalid("图片路径不是普通文件".into()));
        }
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| ClipboardError::Invalid(format!("无法读取本地图片：{error}")))?;
    if !metadata.is_file() {
        return Err(ClipboardError::Invalid("图片路径不是普通文件".into()));
    }
    if metadata.len() > crate::pipeline::MAX_INPUT_BYTES {
        return Err(ClipboardError::TooLarge("本地图片超过 64 MiB"));
    }
    Ok(())
}

struct OwnerWindow(HWND);
impl OwnerWindow {
    fn new() -> ClipboardResult<Self> {
        // Built-in STATIC class, message-only and invisible. A real HWND is
        // required: OpenClipboard(NULL)+EmptyClipboard gives a NULL owner and
        // causes SetClipboardData to fail. No custom callback or thread survives.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                windows_sys::w!("gloamere clipboard"),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            Err(os_error("CreateWindowExW"))
        } else {
            Ok(Self(hwnd))
        }
    }
}
impl Drop for OwnerWindow {
    fn drop(&mut self) {
        // SAFETY: HWND was created on and never leaves this calling thread.
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

struct OwnedMemory(HGLOBAL);
impl OwnedMemory {
    fn from_bytes(bytes: &[u8]) -> ClipboardResult<Self> {
        // Eager allocation and filling happens before touching existing clipboard.
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) };
        if handle.is_null() {
            return Err(os_error("GlobalAlloc"));
        }
        let owned = Self(handle);
        let pointer = unsafe { GlobalLock(handle) } as *mut u8;
        if pointer.is_null() {
            return Err(os_error("GlobalLock"));
        }
        // SAFETY: GlobalAlloc allocated bytes.len() writable bytes, disjoint from
        // the Rust input. Ownership remains with owned until SetClipboardData.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        }
        // GlobalUnlock returns zero on the successful final unlock as well as
        // failure; only a nonzero last-error distinguishes the latter.
        unsafe {
            SetLastError(0);
        }
        if unsafe { GlobalUnlock(handle) } == 0 && unsafe { GetLastError() } != 0 {
            return Err(os_error("GlobalUnlock"));
        }
        Ok(owned)
    }
    fn transfer(mut self, format: u32, _open: &OpenGuard) -> ClipboardResult<()> {
        // SAFETY: clipboard has a live owner, handle is moveable and unlocked.
        if unsafe { SetClipboardData(format, self.0) }.is_null() {
            return Err(write_error("SetClipboardData"));
        }
        // From this point only the OS may free the handle.
        self.0 = ptr::null_mut();
        Ok(())
    }
}
impl Drop for OwnedMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: only allocations not transferred to the OS reach here.
            unsafe {
                GlobalFree(self.0);
            }
        }
    }
}

pub(super) fn copy_pixels(pixels: &RgbaImage, cancel: &crate::Cancellation) -> ClipboardResult<()> {
    cancel.check().map_err(cancellation_error)?;
    let _serial = serial()?;
    let size = codec::rgba_len(pixels.width(), pixels.height())?;
    // Reserve app encoding, DIB and both Win32 blocks before allocating. The
    // source image has its separate owner. OS data escapes application accounting
    // after successful publication, just as any other system clipboard owner.
    let _reservation = crate::memory::reserve(
        2 * MAX_CLIPBOARD_BYTES as u64
            + 2 * (size as u64 + 124)
            + crate::pipeline::MAX_DECODE_BYTES
            + crate::memory::OVERHEAD,
    )
    .map_err(admission)?;
    let png = png_format()?;
    let encoded = codec::encode_png(pixels)?;
    cancel.check().map_err(cancellation_error)?;
    let dib = codec::encode_dibv5(pixels)?;
    cancel.check().map_err(cancellation_error)?;
    let png_memory = OwnedMemory::from_bytes(&encoded)?;
    cancel.check().map_err(cancellation_error)?;
    let dib_memory = OwnedMemory::from_bytes(&dib)?;
    cancel.check().map_err(cancellation_error)?;
    drop(encoded);
    drop(dib);
    let owner = OwnerWindow::new()?;
    let open = OpenGuard::open(owner.0)?;
    // Windows requires EmptyClipboard to publish a new value. Do this only after
    // ALL representations and allocations are ready, immediately before writes.
    publish_copy(cancel, || {
        if unsafe { EmptyClipboard() } == 0 {
            return Err(write_error("EmptyClipboard"));
        }
        png_memory.transfer(png, &open)?;
        dib_memory.transfer(CF_DIBV5 as u32, &open)?;
        open.close(true)
    })
}

pub(super) fn copy_text(text: &str) -> ClipboardResult<()> {
    let _serial = serial()?;
    let _reservation = crate::memory::reserve(MAX_TEXT_BYTES as u64 * 2).map_err(admission)?;
    let text = codec::encode_text(text)?;
    let memory = OwnedMemory::from_bytes(&text)?;
    let owner = OwnerWindow::new()?;
    let open = OpenGuard::open(owner.0)?;
    if unsafe { EmptyClipboard() } == 0 {
        return Err(write_error("EmptyClipboard"));
    }
    memory.transfer(CF_UNICODETEXT as u32, &open)?;
    open.close(true)
}
