//! Opt-in updater. Production activation is deliberately impossible in this
//! development candidate: no deployment trust root or IP HTTPS service is provisioned.
pub mod client;
pub mod engine;
pub mod installer_native;
pub mod integration;
pub mod launch;
pub mod model_client;
pub mod model_update;
pub mod platform;
pub(crate) mod private_paths;
pub mod recovery;
pub mod storage;

pub const PRODUCT_ID: &str = "9CCB6F43-7782-4F69-9E91-67C6D144737B";
pub const UPDATER_PROTOCOL: u32 = 1;
pub const LAUNCHER_PROTOCOL: u32 = 1;
pub const PRODUCTION_DISABLED_REASON: &str = "正式更新尚未开放：IP HTTPS 更新源、可信发布根及 Windows 安装恢复验收尚未配置完成。不会联网下载或自动安装。";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("更新文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("更新状态数据无效：{0}")]
    Json(#[from] serde_json::Error),
    #[error("更新安全检查失败：{0}")]
    Security(String),
    #[error("受保护路径的所有者不符合所需 Windows 主体规则；拒绝继续")]
    PathOwnerMismatch,
    #[error("受保护路径的写入权限不安全或不受支持；拒绝继续")]
    PathUnsafeWriteAccess,
    #[error("更新暂缓：{0}")]
    Deferred(String),
    #[error("更新需要修复：{0}")]
    Repair(String),
}
pub type Result<T> = std::result::Result<T, UpdateError>;
