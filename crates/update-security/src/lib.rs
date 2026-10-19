//! Fail-closed TUF-backed update metadata and bounded HTTPS downloads.
//! Production trust material is intentionally absent. Package verification is
//! not an instruction or permission to execute an installer.
#![deny(unsafe_code)]

mod bootstrap;
mod config;
pub mod download;
#[cfg(feature = "fixture-harness")]
pub mod fixture;
mod metadata;
mod models;
mod state;

pub use config::{
    APP_PACKAGE_LIMIT, METADATA_LIMIT, MODEL_PACKAGE_LIMIT, MODEL_UNPACKED_LIMIT, PRODUCT,
    ProductionConfig, PublisherPolicy,
};
pub use metadata::{
    Compatibility, ReleaseMetadata, TrustedRepository, VerifiedTarget, blocking_check,
    blocking_download, refresh, revalidate_for_execution,
};
pub use models::{
    ModelCompatibility, ModelReleaseMetadata, VerifiedModelTarget, blocking_check_model,
    blocking_download_model, revalidate_model_for_activation,
};
pub use state::{LockedState, MetadataFloor, TrustState};
pub use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("production updates unavailable: {0}")]
    NotConfigured(&'static str),
    #[error("update security policy rejected input: {0}")]
    Policy(&'static str),
    #[error("TUF verification failed: {0}")]
    Tuf(String),
    #[error("anti-rollback rejection: {0}")]
    Rollback(&'static str),
    #[error("persistent trust state is missing, corrupt or inconsistent: {0}")]
    State(&'static str),
    #[error("another update operation holds the trust-state lock")]
    Locked,
    #[error("update was cancelled")]
    Cancelled,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("download failed: {0}")]
    Download(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
