pub mod browsing;
pub mod cli;
pub mod edit;
pub mod export;
pub mod memory;
pub mod pipeline;
pub mod provenance;
pub mod settings;
pub mod worker;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("已取消")]
    Cancelled,
    #[error("资源暂时忙：{0}")]
    ResourceBusy(String),
    #[error("{0}")]
    Invalid(String),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("图像处理失败：{0}")]
    Image(#[from] image::ImageError),
}
pub type Result<T> = std::result::Result<T, ToolError>;

#[derive(Clone, Default)]
pub struct Cancellation(std::sync::Arc<std::sync::atomic::AtomicU8>);
impl Cancellation {
    const CANCELLED: u8 = 1;
    const PUBLICATION_STARTED: u8 = 2;
    const PUBLICATION_FINISHED: u8 = 4;
    pub fn cancel(&self) {
        // Keep ordinary token semantics even when publication won first.
        // The publication winner does not recheck this bit or undo a real success.
        self.0
            .fetch_or(Self::CANCELLED, std::sync::atomic::Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.0.load(std::sync::atomic::Ordering::Acquire) & Self::CANCELLED != 0 {
            Err(ToolError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub(crate) fn publish<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        match self.0.compare_exchange(
            0,
            Self::PUBLICATION_STARTED,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => {
                let result = operation();
                self.0.fetch_or(
                    Self::PUBLICATION_FINISHED,
                    std::sync::atomic::Ordering::Release,
                );
                result
            }
            Err(state) if state & Self::CANCELLED != 0 => Err(ToolError::Cancelled),
            Err(_) => Err(ToolError::Invalid("最终提交已开始或已经结束".into())),
        }
    }
}
