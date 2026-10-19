//! Explicit, bounded clipboard operations. There is no watcher or clipboard history.
//!
//! The Windows implementation preflights global-memory lengths and image dimensions
//! before making an application-owned copy. OS delayed rendering and codec scratch
//! allocations are not a hard process sandbox. See docs/clipboard-v0.5.md.
use image::RgbaImage;
use std::{path::PathBuf, sync::Arc};

#[cfg(any(windows, test))]
mod codec;
#[cfg(windows)]
mod windows;

pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PATH_UNITS: usize = 32_767;
pub const MAX_TEXT_BYTES: usize = (MAX_PATH_UNITS + 1) * 2;

/// Describes the transferred representation, never the original file or author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSource {
    Png,
    DibV5,
    Dib,
}

#[derive(Debug)]
pub struct ClipboardImage {
    pub pixels: Arc<crate::memory::Pixels>,
    pub source: ClipboardSource,
    pub color_status: String,
    pub color_managed: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ClipboardInput {
    Pixels(ClipboardImage),
    /// A single local regular image file; open through the normal bounded pipeline.
    File(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("已取消复制")]
    Cancelled,
    #[error("剪贴板暂时被占用，请稍后重试")]
    Busy,
    #[error("剪贴板中没有支持的图片或单个本地图片路径")]
    NoSupportedFormat,
    #[error("当前平台尚未实现并验证剪贴板图片功能")]
    UnsupportedPlatform,
    #[error("剪贴板数据超出安全限制：{0}")]
    TooLarge(&'static str),
    #[error("剪贴板数据无效：{0}")]
    Invalid(String),
    #[error("暂不支持此剪贴板格式：{0}")]
    UnsupportedData(&'static str),
    #[error("剪贴板包含多个文件，请使用打开对话框选择一张图片")]
    MultipleFiles,
    #[error("仅接受本地普通图片文件；不读取网址、网络盘、设备路径或重解析点")]
    NonLocalPath,
    #[error("剪贴板内存准入失败：{0}")]
    Resource(String),
    #[error("剪贴板系统操作失败（{operation}，错误 {code}），请重试")]
    Platform { operation: &'static str, code: u32 },
    #[error(
        "剪贴板写入未完整完成（{operation}，错误 {code}）；旧内容可能已被替换，请重试或导出 PNG 副本"
    )]
    WriteFailed { operation: &'static str, code: u32 },
}
pub type ClipboardResult<T> = std::result::Result<T, ClipboardError>;

/// All instances share a process-wide, nonblocking operation lock on Windows.
/// A call never waits/retries the OS clipboard lock, steals it, or polls changes.
#[derive(Default)]
pub struct ClipboardAdapter;
impl ClipboardAdapter {
    pub const fn new() -> Self {
        Self
    }

    pub fn read_image_or_file(&mut self) -> ClipboardResult<ClipboardInput> {
        #[cfg(windows)]
        {
            windows::read_image_or_file()
        }
        #[cfg(not(windows))]
        {
            Err(ClipboardError::UnsupportedPlatform)
        }
    }

    /// Copies the complete supplied RGBA image, with no UI, viewport, or metadata.
    pub fn copy_pixels(&mut self, pixels: &RgbaImage) -> ClipboardResult<()> {
        self.copy_pixels_cancellable(pixels, &crate::Cancellation::default())
    }

    /// Cancellation can prevent publication while buffers are prepared. Once
    /// publication wins the token's atomic gate, report the real OS write result.
    pub fn copy_pixels_cancellable(
        &mut self,
        pixels: &RgbaImage,
        cancel: &crate::Cancellation,
    ) -> ClipboardResult<()> {
        #[cfg(windows)]
        {
            windows::copy_pixels(pixels, cancel)
        }
        #[cfg(not(windows))]
        {
            let _ = (pixels, cancel);
            Err(ClipboardError::UnsupportedPlatform)
        }
    }

    /// Used for the separately named Copy Path action; never reads or executes text.
    pub fn copy_text(&mut self, text: &str) -> ClipboardResult<()> {
        #[cfg(windows)]
        {
            windows::copy_text(text)
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Err(ClipboardError::UnsupportedPlatform)
        }
    }
}

#[cfg(any(windows, test))]
fn admission(error: crate::ToolError) -> ClipboardError {
    ClipboardError::Resource(error.to_string())
}

#[cfg(any(windows, test))]
fn cancellation_error(error: crate::ToolError) -> ClipboardError {
    match error {
        crate::ToolError::Cancelled => ClipboardError::Cancelled,
        other => ClipboardError::Invalid(other.to_string()),
    }
}

/// Preserve native error categories across the shared cancellation/publication
/// gate. No fallible operation takes place after a successful native result.
#[cfg(any(windows, test))]
fn publish_copy<T>(
    cancel: &crate::Cancellation,
    operation: impl FnOnce() -> ClipboardResult<T>,
) -> ClipboardResult<T> {
    let mut result = None;
    cancel
        .publish(|| {
            result = Some(operation());
            Ok(())
        })
        .map_err(cancellation_error)?;
    result.expect("successful publication gate invokes its operation exactly once")
}

#[cfg(test)]
mod tests;
