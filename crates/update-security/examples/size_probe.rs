//! Link-size probe only; performs no network request or update operation.
//! Not a shipping application or a production-configuration bypass.
use safeupdate_core::{
    CancellationToken, Compatibility, LockedState, ProductionConfig, Result, VerifiedTarget,
};
use std::path::Path;
fn main() {
    type Check = fn(
        &ProductionConfig,
        &mut LockedState,
        &Compatibility,
        &CancellationToken,
    ) -> Result<Option<VerifiedTarget>>;
    type Download = fn(
        &ProductionConfig,
        &VerifiedTarget,
        &Path,
        &CancellationToken,
    ) -> Result<safeupdate_core::download::VerifiedDownload>;
    std::hint::black_box(safeupdate_core::blocking_check as Check);
    std::hint::black_box(safeupdate_core::blocking_download as Download);
    type CheckModel = fn(
        &ProductionConfig,
        &mut LockedState,
        &safeupdate_core::ModelCompatibility,
        &CancellationToken,
    ) -> Result<Option<safeupdate_core::VerifiedModelTarget>>;
    type DownloadModel = fn(
        &ProductionConfig,
        &safeupdate_core::VerifiedModelTarget,
        &Path,
        &CancellationToken,
    ) -> Result<safeupdate_core::download::VerifiedDownload>;
    std::hint::black_box(safeupdate_core::blocking_check_model as CheckModel);
    std::hint::black_box(safeupdate_core::blocking_download_model as DownloadModel);
    println!("safeupdate-core link probe; no updater action requested");
}
