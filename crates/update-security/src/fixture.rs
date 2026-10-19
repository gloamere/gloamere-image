//! Debug-only signed-repository harness. Not compiled into production releases.
//! This supplies metadata bytes in place of HTTP but uses the exact production
//! TUF/profile/rollback verifier. It never constructs unchecked target authority.
use crate::{
    CancellationToken, Error, LockedState, METADATA_LIMIT, ProductionConfig, Result,
    TrustedRepository,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tough::{Bytes, Transport, TransportError, TransportErrorKind, TransportStream};
use url::Url;

#[cfg(not(debug_assertions))]
compile_error!(
    "fixture-harness is debug-test-only and must not be enabled in a production/release build"
);

/// Verify a bounded in-memory bundle of signed TUF metadata with the caller's
/// ephemeral pinned root and real file-backed monotonic state. Keys are filenames
/// such as `timestamp.json`, `1.snapshot.json`, `1.targets.json`, `2.root.json`.
/// Missing successive root versions are treated as 404 by the unchanged verifier.
/// Returns a real repository; call `select_model`/`select_app` for opaque targets.
pub fn verify_fixture_repository(
    config: &ProductionConfig,
    state: &mut LockedState,
    metadata: BTreeMap<String, Vec<u8>>,
    cancel: &CancellationToken,
) -> Result<TrustedRepository> {
    if metadata.is_empty() || metadata.len() > 24 {
        return Err(Error::Policy("fixture metadata count limit"));
    }
    let mut total = 0u64;
    for (name, bytes) in &metadata {
        let valid = name == "timestamp.json"
            || [".root.json", ".snapshot.json", ".targets.json"]
                .iter()
                .any(|suffix| {
                    name.strip_suffix(suffix).is_some_and(|version| {
                        !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
                    })
                });
        if !valid || bytes.is_empty() || bytes.len() as u64 > METADATA_LIMIT {
            return Err(Error::Policy("invalid or oversized fixture metadata"));
        }
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Policy("fixture byte overflow"))?;
        if total > 24 * METADATA_LIMIT {
            return Err(Error::Policy("fixture aggregate byte limit"));
        }
    }
    let transport = FixtureTransport {
        metadata: Arc::new(metadata),
        base: config.metadata_url.clone(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(crate::metadata::refresh_with_transport(
        config, state, cancel, transport,
    ))
}

#[derive(Clone, Debug)]
struct FixtureTransport {
    metadata: Arc<BTreeMap<String, Vec<u8>>>,
    base: Url,
}
#[tough::async_trait]
impl Transport for FixtureTransport {
    async fn fetch(&self, url: Url) -> std::result::Result<TransportStream, TransportError> {
        if url.origin() != self.base.origin() {
            return Err(TransportError::new(
                TransportErrorKind::UnsupportedUrlScheme,
                &url,
            ));
        }
        let name = url
            .path()
            .strip_prefix(self.base.path())
            .ok_or_else(|| TransportError::new(TransportErrorKind::Other, &url))?;
        let bytes = self
            .metadata
            .get(name)
            .cloned()
            .ok_or_else(|| TransportError::new(TransportErrorKind::FileNotFound, &url))?;
        Ok(Box::pin(futures_util::stream::iter([Ok(Bytes::from(
            bytes,
        ))])))
    }
}
