//! Strict HTTPS-only metadata transport and signed-length/SHA-256 artifact downloads.
//!
//! The caller supplies trust-verified target metadata and a private, user-owned cache
//! directory. No downloaded bytes are executed. See `docs/download-design.md`.

use futures_util::{Stream, StreamExt};
use reqwest::{
    StatusCode,
    header::{self, HeaderMap},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Debug,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use tough::{Bytes, Transport, TransportError, TransportErrorKind, TransportStream};
use url::Url;

const METADATA_HARD_CAP: u64 = 1024 * 1024;
const SIDECAR_CAP: u64 = 4096;
const PREFIX_CAP: usize = 512;

#[derive(Debug, Clone)]
pub struct TransportLimits {
    /// Metadata is always limited to at most 1 MiB, including for root rotation.
    pub max_response_bytes: u64,
    pub max_target_bytes: u64,
    pub max_redirects: usize,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub total_timeout: Duration,
}
impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: METADATA_HARD_CAP,
            max_target_bytes: 1024 * 1024 * 1024,
            max_redirects: 3,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(20),
            total_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("HTTPS update origins are not configured")]
    NotConfigured,
    #[error("invalid transport limits")]
    InvalidLimits,
    #[error("disallowed update URL: {0}")]
    UrlPolicy(String),
    #[error("too many update redirects")]
    RedirectLimit,
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("invalid or ambiguous response header: {0}")]
    Header(&'static str),
    #[error("encoded responses are forbidden")]
    ContentEncoding,
    #[error("HTML is not update data")]
    Html,
    #[error("response exceeds the allowed length")]
    TooLarge,
    #[error("download length mismatch: expected {expected}, received {actual}")]
    Length { expected: u64, actual: u64 },
    #[error("download SHA-256 does not match signed metadata")]
    Hash,
    #[error("invalid partial response range")]
    Range,
    #[error("resumed object ETag changed or is missing")]
    ChangedObject,
    #[error("download cancelled")]
    Cancelled,
    #[error("download deadline or read timeout elapsed")]
    Timeout,
    #[error("download is already in progress")]
    Busy,
    #[error("unsafe local download path")]
    UnsafePath,
    #[error("destination already exists")]
    DestinationExists,
    #[error("HTTP transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("local download I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid resume state: {0}")]
    Resume(#[from] serde_json::Error),
}
impl DownloadError {
    fn permits_resume(&self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Timeout | Self::Http(_) | Self::Io(_)
        )
    }
}

#[derive(Debug, Clone)]
pub struct HttpsPolicy {
    origins: BTreeSet<String>,
    limits: TransportLimits,
}
impl HttpsPolicy {
    /// Accept exact HTTPS origins, such as `https://updates.example` or an
    /// explicitly configured literal IP. Normal TLS hostname/IP SAN validation
    /// remains mandatory. No origin is enabled by default.
    pub fn new(origins: Vec<String>, limits: TransportLimits) -> Result<Self, DownloadError> {
        if origins.is_empty() {
            return Err(DownloadError::NotConfigured);
        }
        if limits.max_response_bytes == 0
            || limits.max_response_bytes > METADATA_HARD_CAP
            || limits.max_target_bytes == 0
            || limits.max_redirects > 10
            || limits.connect_timeout.is_zero()
            || limits.read_timeout.is_zero()
            || limits.total_timeout.is_zero()
            || limits.connect_timeout > limits.total_timeout
            || limits.read_timeout > limits.total_timeout
        {
            return Err(DownloadError::InvalidLimits);
        }
        let mut normalized = BTreeSet::new();
        for origin in origins {
            validate_raw_reference(&origin)?;
            let url = Url::parse(&origin)
                .map_err(|_| DownloadError::UrlPolicy("invalid origin".into()))?;
            basic_url_check(&url)?;
            if url.path() != "/" {
                return Err(DownloadError::UrlPolicy("origin contains a path".into()));
            }
            normalized.insert(url.origin().ascii_serialization());
        }
        Ok(Self {
            origins: normalized,
            limits,
        })
    }
    pub fn limits(&self) -> &TransportLimits {
        &self.limits
    }
    pub fn parse_url(&self, raw: &str) -> Result<Url, DownloadError> {
        validate_raw_reference(raw)?;
        let url = Url::parse(raw).map_err(|_| DownloadError::UrlPolicy("invalid URL".into()))?;
        self.validate_url(&url)?;
        Ok(url)
    }
    pub fn validate_url(&self, url: &Url) -> Result<(), DownloadError> {
        basic_url_check(url)?;
        if !self.origins.contains(&url.origin().ascii_serialization()) {
            return Err(DownloadError::UrlPolicy("origin is not allowlisted".into()));
        }
        Ok(())
    }
    fn redirect(&self, from: &Url, location: &str) -> Result<Url, DownloadError> {
        validate_raw_reference(location)?;
        let next = from
            .join(location)
            .map_err(|_| DownloadError::UrlPolicy("invalid redirect".into()))?;
        self.validate_url(&next)?;
        Ok(next)
    }
}

// Reject parser-normalized traversal before parsing. Percent-encoded paths are
// deliberately unsupported, preventing escaped and double-escaped separators.
fn validate_raw_reference(raw: &str) -> Result<(), DownloadError> {
    if raw.is_empty()
        || raw
            .bytes()
            .any(|b| b <= b' ' || b >= 127 || matches!(b, b'\\' | b'%' | b'?' | b'#' | b'@'))
        || raw.split('/').any(|segment| matches!(segment, "." | ".."))
    {
        return Err(DownloadError::UrlPolicy(
            "unsafe URL characters or traversal".into(),
        ));
    }
    Ok(())
}
fn basic_url_check(url: &Url) -> Result<(), DownloadError> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(DownloadError::UrlPolicy(
            "HTTPS without credentials, query or fragment required".into(),
        ));
    }
    validate_raw_reference(url.as_str())
}

type Body = Pin<Box<dyn Stream<Item = Result<Bytes, DownloadError>> + Send + Sync>>;
#[derive(Debug, Clone)]
struct HttpRequest {
    url: Url,
    range: Option<(u64, String)>,
}
struct HttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}
#[tough::async_trait]
trait HttpBackend: Debug + Send + Sync {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, DownloadError>;
}
#[derive(Debug)]
struct ReqwestBackend {
    client: reqwest::Client,
}
impl ReqwestBackend {
    fn new(limits: &TransportLimits) -> Result<Self, DownloadError> {
        // Share the maintained AWS-LC provider already required by tough; avoid
        // pulling a second crypto implementation solely for reqwest. A pre-set
        // process provider is respected. No certificate validation is disabled.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .referer(false)
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .connect_timeout(limits.connect_timeout)
                .read_timeout(limits.read_timeout)
                .timeout(limits.total_timeout)
                .user_agent("gloamere-image-safeupdate/0.7")
                .build()?,
        })
    }
}
#[tough::async_trait]
impl HttpBackend for ReqwestBackend {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, DownloadError> {
        let mut builder = self
            .client
            .get(request.url)
            .header(header::ACCEPT_ENCODING, "identity");
        if let Some((offset, etag)) = request.range {
            builder = builder
                .header(header::RANGE, format!("bytes={offset}-"))
                .header(header::IF_RANGE, etag);
        }
        let response = builder.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = Box::pin(
            response
                .bytes_stream()
                .map(|r| r.map_err(DownloadError::Http)),
        );
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

async fn guarded<F: Future>(
    future: F,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<F::Output, DownloadError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(DownloadError::Cancelled),
        result = tokio::time::timeout_at(deadline, future) => result.map_err(|_| DownloadError::Timeout),
    }
}
async fn next_chunk(
    body: &mut Body,
    cancel: &CancellationToken,
    deadline: Instant,
    read_timeout: Duration,
) -> Result<Option<Bytes>, DownloadError> {
    let read_deadline = std::cmp::min(deadline, Instant::now() + read_timeout);
    guarded(body.next(), cancel, read_deadline)
        .await?
        .transpose()
}
async fn request(
    backend: &dyn HttpBackend,
    policy: &HttpsPolicy,
    url: Url,
    range: Option<(u64, String)>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<HttpResponse, DownloadError> {
    let mut current = url;
    for redirects in 0..=policy.limits.max_redirects {
        policy.validate_url(&current)?;
        let response = guarded(
            backend.get(HttpRequest {
                url: current.clone(),
                range: range.clone(),
            }),
            cancel,
            deadline,
        )
        .await??;
        if matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            if redirects == policy.limits.max_redirects {
                return Err(DownloadError::RedirectLimit);
            }
            let location = single_header(&response.headers, header::LOCATION)?
                .ok_or(DownloadError::Header("Location"))?;
            current = policy.redirect(&current, location)?;
            // Dropping redirect bodies avoids consuming unbounded untrusted data.
            continue;
        }
        return Ok(response);
    }
    Err(DownloadError::RedirectLimit)
}
fn single_header(
    headers: &HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, DownloadError> {
    let mut values = headers.get_all(&name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(DownloadError::Header("duplicate header"));
    }
    first
        .map(|v| {
            v.to_str()
                .map_err(|_| DownloadError::Header("non-ASCII header"))
        })
        .transpose()
}
fn content_length(headers: &HeaderMap) -> Result<Option<u64>, DownloadError> {
    single_header(headers, header::CONTENT_LENGTH)?
        .map(|s| {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(DownloadError::Header("Content-Length"));
            }
            s.parse()
                .map_err(|_| DownloadError::Header("Content-Length"))
        })
        .transpose()
}
fn validate_content(headers: &HeaderMap) -> Result<(), DownloadError> {
    if let Some(encoding) = single_header(headers, header::CONTENT_ENCODING)?
        && !encoding.eq_ignore_ascii_case("identity")
    {
        return Err(DownloadError::ContentEncoding);
    }
    if let Some(mime) = single_header(headers, header::CONTENT_TYPE)? {
        let mime = mime.split(';').next().unwrap_or("").trim();
        if mime.eq_ignore_ascii_case("text/html")
            || mime.eq_ignore_ascii_case("application/xhtml+xml")
        {
            return Err(DownloadError::Html);
        }
    }
    Ok(())
}
fn strong_etag(headers: &HeaderMap) -> Result<Option<String>, DownloadError> {
    Ok(single_header(headers, header::ETAG)?
        .filter(|tag| is_strong_etag(tag))
        .map(str::to_owned))
}
fn is_strong_etag(tag: &str) -> bool {
    tag.len() >= 2
        && tag.len() <= 1024
        && tag.starts_with('"')
        && tag.ends_with('"')
        && tag.as_bytes()[1..tag.len() - 1]
            .iter()
            .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b))
}
fn inspect_prefix(prefix: &mut Vec<u8>, chunk: &[u8]) -> Result<(), DownloadError> {
    prefix.extend_from_slice(&chunk[..chunk.len().min(PREFIX_CAP.saturating_sub(prefix.len()))]);
    let mut bytes = prefix.as_slice();
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        bytes = &bytes[3..];
    }
    while let Some((b, rest)) = bytes.split_first() {
        if !b.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    // Installer packages and JSON do not begin with markup. Reject comments
    // and XML prologs too, rather than accepting HTML hidden behind either.
    if bytes.is_empty() && prefix.len() == PREFIX_CAP {
        return Err(DownloadError::Html);
    }
    if [
        b"<!doctype html".as_slice(),
        b"<html",
        b"<head",
        b"<body",
        b"<!--",
        b"<?xml",
    ]
    .iter()
    .any(|tag| {
        bytes
            .get(..tag.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(tag))
    }) {
        return Err(DownloadError::Html);
    }
    Ok(())
}

/// A real `tough` transport with fail-closed HTTPS/origin policy and a fixed
/// metadata cap. TUF remains responsible for signatures, freshness and rollback.
#[derive(Debug, Clone)]
pub struct HttpsTransport {
    policy: HttpsPolicy,
    backend: Arc<dyn HttpBackend>,
    cancel: CancellationToken,
}
impl HttpsTransport {
    pub fn new(policy: HttpsPolicy, cancel: CancellationToken) -> Result<Self, DownloadError> {
        let backend = Arc::new(ReqwestBackend::new(policy.limits())?);
        Ok(Self {
            policy,
            backend,
            cancel,
        })
    }
    async fn fetch_bounded(&self, url: Url) -> Result<Vec<u8>, DownloadError> {
        let deadline = Instant::now() + self.policy.limits.total_timeout;
        let mut response = request(
            self.backend.as_ref(),
            &self.policy,
            url,
            None,
            &self.cancel,
            deadline,
        )
        .await?;
        if response.status != StatusCode::OK {
            return Err(DownloadError::HttpStatus(response.status.as_u16()));
        }
        validate_content(&response.headers)?;
        if single_header(&response.headers, header::CONTENT_RANGE)?.is_some() {
            return Err(DownloadError::Range);
        }
        let advertised = content_length(&response.headers)?;
        let cap = self.policy.limits.max_response_bytes;
        if advertised.is_some_and(|length| length > cap) {
            return Err(DownloadError::TooLarge);
        }
        let mut bytes = Vec::new();
        let mut prefix = Vec::new();
        while let Some(chunk) = next_chunk(
            &mut response.body,
            &self.cancel,
            deadline,
            self.policy.limits.read_timeout,
        )
        .await?
        {
            if (chunk.len() as u64) > cap.saturating_sub(bytes.len() as u64) {
                return Err(DownloadError::TooLarge);
            }
            inspect_prefix(&mut prefix, &chunk)?;
            bytes.extend_from_slice(&chunk);
        }
        if let Some(expected) = advertised
            && bytes.len() as u64 != expected
        {
            return Err(DownloadError::Length {
                expected,
                actual: bytes.len() as u64,
            });
        }
        Ok(bytes)
    }
}
#[tough::async_trait]
impl Transport for HttpsTransport {
    async fn fetch(&self, url: Url) -> Result<TransportStream, TransportError> {
        let bytes = self.fetch_bounded(url.clone()).await.map_err(|error| {
            let kind = match error {
                DownloadError::HttpStatus(404) => TransportErrorKind::FileNotFound,
                DownloadError::UrlPolicy(_) => TransportErrorKind::UnsupportedUrlScheme,
                _ => TransportErrorKind::Other,
            };
            TransportError::new_with_cause(kind, url, error)
        })?;
        Ok(Box::pin(futures_util::stream::iter([Ok(Bytes::from(
            bytes,
        ))])))
    }
}

/// Integrity values supplied by the trust-verifying metadata layer. This type
/// alone is not evidence that metadata signatures have been verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedTarget {
    pub length: u64,
    pub sha256: [u8; 32],
}
#[derive(Debug, Clone)]
pub struct VerifiedDownload {
    pub path: PathBuf,
    pub length: u64,
    pub sha256: [u8; 32],
    pub resumed_from: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeState {
    url: String,
    length: u64,
    sha256: [u8; 32],
    etag: String,
}
#[derive(Debug, Clone)]
pub struct Downloader {
    policy: HttpsPolicy,
    backend: Arc<dyn HttpBackend>,
}
impl Downloader {
    pub fn new(policy: HttpsPolicy) -> Result<Self, DownloadError> {
        let backend = Arc::new(ReqwestBackend::new(policy.limits())?);
        Ok(Self { policy, backend })
    }
    pub async fn download(
        &self,
        url: &str,
        expected: &ExpectedTarget,
        destination: &Path,
        cancel: &CancellationToken,
    ) -> Result<VerifiedDownload, DownloadError> {
        let url = self.policy.parse_url(url)?;
        if expected.length > self.policy.limits.max_target_bytes {
            return Err(DownloadError::TooLarge);
        }
        let deadline = Instant::now() + self.policy.limits.total_timeout;
        guarded(async {}, cancel, deadline).await?;
        let paths = DownloadPaths::new(destination)?;
        paths.validate_directory()?;
        let _lock = DownloadLock::acquire(&paths.lock)?;
        if regular_file_size(destination)?.is_some() {
            return Err(DownloadError::DestinationExists);
        }
        regular_file_size(&paths.part)?;
        regular_file_size(&paths.state)?;
        let result = self
            .download_locked(&url, expected, &paths, cancel, deadline)
            .await;
        if let Err(error) = &result {
            // Only transport/I/O interruptions can leave a resumable partial.
            // Policy/integrity failures must never become future resume input.
            if !error.permits_resume() || regular_file_size(&paths.state)?.is_none() {
                paths.clear_partial()?;
            }
        }
        result
    }
    async fn download_locked(
        &self,
        url: &Url,
        expected: &ExpectedTarget,
        paths: &DownloadPaths,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<VerifiedDownload, DownloadError> {
        let mut resume = read_resume(paths, url, expected, cancel, deadline).await?;
        let mut offset = if resume.is_some() {
            regular_file_size(&paths.part)?.unwrap_or(0)
        } else {
            0
        };
        if offset > expected.length {
            paths.clear_partial()?;
            return Err(DownloadError::TooLarge);
        }
        let mut hash = Sha256::new();
        let mut prefix = Vec::new();
        if offset > 0 {
            let mut previous =
                guarded(tokio::fs::File::open(&paths.part), cancel, deadline).await??;
            let mut count = 0u64;
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let size = guarded(previous.read(&mut buffer), cancel, deadline).await??;
                if size == 0 {
                    break;
                }
                count = count
                    .checked_add(size as u64)
                    .ok_or(DownloadError::TooLarge)?;
                if count > offset {
                    return Err(DownloadError::TooLarge);
                }
                inspect_prefix(&mut prefix, &buffer[..size])?;
                hash.update(&buffer[..size]);
            }
            if count != offset {
                return Err(DownloadError::Length {
                    expected: offset,
                    actual: count,
                });
            }
        }
        let mut resumed_from = offset;
        if offset == expected.length && resume.is_some() {
            return finish(paths, expected, hash, resumed_from, cancel, deadline).await;
        }
        let range = resume
            .as_ref()
            .filter(|_| offset > 0)
            .map(|state| (offset, state.etag.clone()));
        let mut response = request(
            self.backend.as_ref(),
            &self.policy,
            url.clone(),
            range,
            cancel,
            deadline,
        )
        .await?;
        validate_content(&response.headers)?;
        let advertised = content_length(&response.headers)?;
        let etag = strong_etag(&response.headers)?;
        if offset > 0 && response.status == StatusCode::PARTIAL_CONTENT {
            if etag.as_deref() != resume.as_ref().map(|state| state.etag.as_str()) {
                return Err(DownloadError::ChangedObject);
            }
            validate_range(&response.headers, offset, expected.length)?;
            if advertised.is_some_and(|n| n != expected.length - offset) {
                return Err(DownloadError::Range);
            }
        } else if response.status == StatusCode::OK {
            if single_header(&response.headers, header::CONTENT_RANGE)?.is_some() {
                return Err(DownloadError::Range);
            }
            if advertised.is_some_and(|n| n != expected.length) {
                return Err(DownloadError::Length {
                    expected: expected.length,
                    actual: advertised.unwrap_or(0),
                });
            }
            // A server may ignore Range or the object may have changed. A full
            // 200 response is safe only after discarding every prior byte.
            offset = 0;
            resumed_from = 0;
            hash = Sha256::new();
            prefix.clear();
            resume = None;
            paths.clear_partial()?;
        } else {
            return Err(if response.status == StatusCode::PARTIAL_CONTENT {
                DownloadError::Range
            } else {
                DownloadError::HttpStatus(response.status.as_u16())
            });
        }
        if let Some(etag) = etag {
            let state = ResumeState {
                url: url.as_str().to_owned(),
                length: expected.length,
                sha256: expected.sha256,
                etag,
            };
            // Bound and write before appending. The on-disk file length is the
            // only resume offset; every saved byte is rehashed on the next run.
            let json = serde_json::to_vec(&state)?;
            if json.len() as u64 > SIDECAR_CAP {
                return Err(DownloadError::TooLarge);
            }
            guarded(async {}, cancel, deadline).await?;
            paths.validate_directory()?;
            regular_file_size(&paths.state)?;
            tokio::fs::write(&paths.state, json).await?;
            guarded(async {}, cancel, deadline).await?;
        } else if resume.is_none() {
            remove_if_present(&paths.state)?;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true);
        if offset > 0 {
            options.append(true);
        } else {
            options.truncate(true);
        }
        guarded(async {}, cancel, deadline).await?;
        paths.validate_directory()?;
        regular_file_size(&paths.part)?;
        let mut output = options.open(&paths.part).await?;
        guarded(async {}, cancel, deadline).await?;
        while let Some(chunk) = next_chunk(
            &mut response.body,
            cancel,
            deadline,
            self.policy.limits.read_timeout,
        )
        .await?
        {
            if chunk.len() as u64 > expected.length.saturating_sub(offset) {
                return Err(DownloadError::TooLarge);
            }
            inspect_prefix(&mut prefix, &chunk)?;
            // Local writes are drained before observing cancellation so a
            // dropped Tokio file operation cannot modify a new caller's part.
            guarded(async {}, cancel, deadline).await?;
            output.write_all(&chunk).await?;
            output.flush().await?;
            guarded(async {}, cancel, deadline).await?;
            hash.update(&chunk);
            offset += chunk.len() as u64;
        }
        if offset != expected.length {
            return Err(DownloadError::Length {
                expected: expected.length,
                actual: offset,
            });
        }
        output.flush().await?;
        output.sync_all().await?;
        guarded(async {}, cancel, deadline).await?;
        drop(output);
        finish(paths, expected, hash, resumed_from, cancel, deadline).await
    }
}
fn validate_range(headers: &HeaderMap, offset: u64, total: u64) -> Result<(), DownloadError> {
    let actual = single_header(headers, header::CONTENT_RANGE)?.ok_or(DownloadError::Range)?;
    let end = total.checked_sub(1).ok_or(DownloadError::Range)?;
    if offset >= total || actual != format!("bytes {offset}-{end}/{total}") {
        return Err(DownloadError::Range);
    }
    Ok(())
}

struct DownloadPaths {
    destination: PathBuf,
    part: PathBuf,
    state: PathBuf,
    lock: PathBuf,
    #[cfg(unix)]
    owner_uid: u32,
}
impl DownloadPaths {
    fn new(destination: &Path) -> Result<Self, DownloadError> {
        if !destination.is_absolute()
            || destination.file_name().is_none()
            || destination.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(DownloadError::UnsafePath);
        }
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if !matches!(destination.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            {
                return Err(DownloadError::UnsafePath);
            }
            for part in destination.components() {
                if let Component::Normal(name) = part {
                    let name = name.to_str().ok_or(DownloadError::UnsafePath)?;
                    if name.ends_with(['.', ' '])
                        || name.bytes().any(|b| {
                            b < 32 || matches!(b, b':' | b'<' | b'>' | b'"' | b'|' | b'?' | b'*')
                        })
                    {
                        return Err(DownloadError::UnsafePath);
                    }
                }
            }
        }
        // An anonymous file's owner is the effective OS user, without unsafe
        // libc calls or trusting USER/HOME environment variables.
        #[cfg(unix)]
        let owner_uid = {
            use std::os::unix::fs::MetadataExt;
            tempfile::tempfile()?.metadata()?.uid()
        };
        let sibling = |suffix: &str| {
            let mut name = destination.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        };
        let paths = Self {
            destination: destination.into(),
            part: sibling(".part"),
            state: sibling(".part.json"),
            lock: sibling(".download.lock"),
            #[cfg(unix)]
            owner_uid,
        };
        paths.validate_directory()?;
        Ok(paths)
    }
    fn validate_directory(&self) -> Result<(), DownloadError> {
        let parent = self.destination.parent().ok_or(DownloadError::UnsafePath)?;
        for ancestor in parent.ancestors() {
            let metadata = std::fs::symlink_metadata(ancestor)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(DownloadError::UnsafePath);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(DownloadError::UnsafePath);
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let mode = metadata.mode();
                if (mode & 0o022 != 0 && mode & 0o1000 == 0)
                    || (ancestor == parent
                        && (metadata.uid() != self.owner_uid || mode & 0o077 != 0))
                {
                    return Err(DownloadError::UnsafePath);
                }
            }
        }
        Ok(())
    }
    fn clear_partial(&self) -> Result<(), DownloadError> {
        self.validate_directory()?;
        remove_if_present(&self.part)?;
        remove_if_present(&self.state)
    }
}
fn regular_file_size(path: &Path) -> Result<Option<u64>, DownloadError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(DownloadError::UnsafePath);
                }
            }
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(DownloadError::UnsafePath);
            }
            Ok(Some(metadata.len()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn remove_if_present(path: &Path) -> Result<(), DownloadError> {
    if regular_file_size(path)?.is_some() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
struct DownloadLock {
    _file: std::fs::File,
}
impl DownloadLock {
    fn acquire(path: &Path) -> Result<Self, DownloadError> {
        regular_file_size(path)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => DownloadError::Busy,
            std::fs::TryLockError::Error(error) => DownloadError::Io(error),
        })?;
        // Keep the lock inode stable. OS locks release automatically on close
        // or process exit, so a crashed download does not strand the cache.
        Ok(Self { _file: file })
    }
}
async fn read_resume(
    paths: &DownloadPaths,
    url: &Url,
    expected: &ExpectedTarget,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Option<ResumeState>, DownloadError> {
    paths.validate_directory()?;
    let length = regular_file_size(&paths.part)?;
    let state_length = regular_file_size(&paths.state)?;
    if length.is_none() || state_length.is_none_or(|size| size > SIDECAR_CAP) {
        paths.clear_partial()?;
        return Ok(None);
    }
    let file = guarded(tokio::fs::File::open(&paths.state), cancel, deadline).await??;
    let mut bytes = Vec::new();
    guarded(
        file.take(SIDECAR_CAP + 1).read_to_end(&mut bytes),
        cancel,
        deadline,
    )
    .await??;
    if bytes.len() as u64 > SIDECAR_CAP {
        paths.clear_partial()?;
        return Ok(None);
    }
    let state: ResumeState = match serde_json::from_slice(&bytes) {
        Ok(state) => state,
        Err(_) => {
            paths.clear_partial()?;
            return Ok(None);
        }
    };
    if state.url != url.as_str()
        || state.length != expected.length
        || state.sha256 != expected.sha256
        || !is_strong_etag(&state.etag)
    {
        paths.clear_partial()?;
        return Ok(None);
    }
    Ok(Some(state))
}
async fn finish(
    paths: &DownloadPaths,
    expected: &ExpectedTarget,
    hash: Sha256,
    resumed_from: u64,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<VerifiedDownload, DownloadError> {
    let actual: [u8; 32] = hash.finalize().into();
    if actual != expected.sha256 {
        return Err(DownloadError::Hash);
    }
    guarded(async {}, cancel, deadline).await?;
    paths.validate_directory()?;
    if regular_file_size(&paths.part)? != Some(expected.length) {
        return Err(DownloadError::UnsafePath);
    }
    if regular_file_size(&paths.destination)?.is_some() {
        return Err(DownloadError::DestinationExists);
    }
    // Sync even when finalizing a previously complete, interrupted partial.
    let file = guarded(
        tokio::fs::OpenOptions::new().write(true).open(&paths.part),
        cancel,
        deadline,
    )
    .await??;
    file.sync_all().await?;
    guarded(async {}, cancel, deadline).await?;
    drop(file);
    // Cancellation must not detach a rename and report failure after promotion.
    // This bounded local metadata operation is synchronous under the lock.
    guarded(async {}, cancel, deadline).await?;
    paths.validate_directory()?;
    std::fs::rename(&paths.part, &paths.destination)?;
    remove_if_present(&paths.state)?;
    Ok(VerifiedDownload {
        path: paths.destination.clone(),
        length: expected.length,
        sha256: actual,
        resumed_from,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    const ORIGIN: &str = "https://updates.example.test";
    const TARGET: &str = "https://updates.example.test/targets/app.exe";
    struct Scripted {
        responses: Mutex<VecDeque<HttpResponse>>,
        requests: Mutex<Vec<HttpRequest>>,
    }
    impl Debug for Scripted {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("ScriptedHttp")
        }
    }
    #[tough::async_trait]
    impl HttpBackend for Scripted {
        async fn get(&self, request: HttpRequest) -> Result<HttpResponse, DownloadError> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| DownloadError::Io(std::io::Error::other("no scripted response")))
        }
    }
    fn private_tempdir() -> tempfile::TempDir {
        let builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut builder = builder;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
            return builder.tempdir().unwrap();
        }
        #[cfg(not(unix))]
        builder.tempdir().unwrap()
    }
    fn policy() -> HttpsPolicy {
        HttpsPolicy::new(vec![ORIGIN.into()], TransportLimits::default()).unwrap()
    }
    fn response(status: u16, headers: &[(&str, &str)], chunks: &[&[u8]]) -> HttpResponse {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                header::HeaderValue::from_str(value).unwrap(),
            );
        }
        HttpResponse {
            status: StatusCode::from_u16(status).unwrap(),
            headers: map,
            body: Box::pin(futures_util::stream::iter(
                chunks
                    .iter()
                    .map(|c| Ok(Bytes::copy_from_slice(c)))
                    .collect::<Vec<_>>(),
            )),
        }
    }
    fn backend(responses: Vec<HttpResponse>) -> Arc<Scripted> {
        Arc::new(Scripted {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn downloader(responses: Vec<HttpResponse>) -> (Downloader, Arc<Scripted>) {
        let backend = backend(responses);
        (
            Downloader {
                policy: policy(),
                backend: backend.clone(),
            },
            backend,
        )
    }
    fn expected(data: &[u8]) -> ExpectedTarget {
        ExpectedTarget {
            length: data.len() as u64,
            sha256: Sha256::digest(data).into(),
        }
    }
    fn seed(destination: &Path, prefix: &[u8], expected: &ExpectedTarget, etag: &str) {
        let paths = DownloadPaths::new(destination).unwrap();
        std::fs::write(paths.part, prefix).unwrap();
        std::fs::write(
            paths.state,
            serde_json::to_vec(&ResumeState {
                url: TARGET.into(),
                length: expected.length,
                sha256: expected.sha256,
                etag: etag.into(),
            })
            .unwrap(),
        )
        .unwrap();
    }
    fn clean_partial(destination: &Path) -> bool {
        let p = DownloadPaths::new(destination).unwrap();
        !p.part.exists() && !p.state.exists() && !destination.exists()
    }

    #[test]
    fn origins_fail_closed_and_metadata_cap_is_absolute() {
        assert!(matches!(
            HttpsPolicy::new(vec![], TransportLimits::default()),
            Err(DownloadError::NotConfigured)
        ));
        for bad in [
            "http://updates.example.test",
            "https://updates.example.test/path",
            "https://user@updates.example.test",
            "https://updates.example.test:0",
            "file:///tmp",
        ] {
            assert!(
                HttpsPolicy::new(vec![bad.into()], TransportLimits::default()).is_err(),
                "{bad}"
            );
        }
        let limits = TransportLimits {
            max_response_bytes: METADATA_HARD_CAP + 1,
            ..TransportLimits::default()
        };
        assert!(matches!(
            HttpsPolicy::new(vec![ORIGIN.into()], limits),
            Err(DownloadError::InvalidLimits)
        ));
    }
    #[test]
    fn url_policy_rejects_escaped_and_normalized_traversal() {
        let policy = policy();
        assert!(policy.parse_url(TARGET).is_ok());
        for bad in [
            "http://updates.example.test/a",
            "https://evil.example/a",
            "https://updates.example.test:444/a",
            "https://user@updates.example.test/a",
            "https://@updates.example.test/a",
            "https://updates.example.test/a?q=1",
            "https://updates.example.test/a#x",
            "https://updates.example.test/a/../b",
            "https://updates.example.test/a/./b",
            "https://updates.example.test/%2e%2e/a",
            "https://updates.example.test/%252fetc",
            "https://updates.example.test/a\\b",
            "https://updates.example.test/a\nb",
        ] {
            assert!(policy.parse_url(bad).is_err(), "accepted {bad}");
        }
    }
    #[test]
    fn explicit_ip_origin_is_allowed_without_tls_override() {
        let p = HttpsPolicy::new(
            vec!["https://175.24.190.175".into()],
            TransportLimits::default(),
        )
        .unwrap();
        assert!(
            p.parse_url("https://175.24.190.175/metadata/root.json")
                .is_ok()
        );
        assert!(
            p.parse_url("http://175.24.190.175/metadata/root.json")
                .is_err()
        );
        assert!(
            p.parse_url("https://175.24.190.176/metadata/root.json")
                .is_err()
        );
    }
    #[test]
    fn production_clients_construct_with_tls_validation_enabled() {
        // Construction performs no I/O and does not expose a test TLS bypass.
        assert!(Downloader::new(policy()).is_ok());
        assert!(HttpsTransport::new(policy(), CancellationToken::new()).is_ok());
    }
    #[test]
    fn markup_comments_xml_and_excess_leading_whitespace_are_rejected() {
        for body in [
            b"<!-- login page -->".as_slice(),
            b"<?xml version=\"1.0\"?><html>",
            &[b' '; PREFIX_CAP],
        ] {
            assert!(matches!(
                inspect_prefix(&mut Vec::new(), body),
                Err(DownloadError::Html)
            ));
        }
    }
    #[test]
    fn only_strong_etags_qualify_for_resume() {
        for good in ["\"abc\"", "\"\"", "\"a!#$%&'()*+,-./:;<=>?@[\\]^_`{|}~\""] {
            assert!(is_strong_etag(good));
        }
        for bad in [
            "W/\"abc\"",
            "abc",
            "\"a\"b\"",
            "\"a b\"",
            "\"a\nb\"",
            "\"é\"",
        ] {
            assert!(!is_strong_etag(bad), "{bad}");
        }
    }
    #[test]
    fn content_headers_fail_closed() {
        for headers in [
            vec![("content-type", "text/html; charset=utf-8")],
            vec![("content-type", "application/xhtml+xml")],
            vec![("content-encoding", "gzip")],
            vec![
                ("content-encoding", "identity"),
                ("content-encoding", "identity"),
            ],
        ] {
            assert!(validate_content(&response(200, &headers, &[]).headers).is_err());
        }
        assert!(
            content_length(
                &response(
                    200,
                    &[("content-length", "1"), ("content-length", "1")],
                    &[]
                )
                .headers
            )
            .is_err()
        );
        assert!(
            content_length(
                &response(200, &[("content-length", "18446744073709551616")], &[]).headers
            )
            .is_err()
        );
    }
    #[test]
    fn os_lock_releases_without_deleting_inode() {
        let temp = private_tempdir();
        let path = temp.path().join("file.lock");
        let lock = DownloadLock::acquire(&path).unwrap();
        assert!(matches!(
            DownloadLock::acquire(&path),
            Err(DownloadError::Busy)
        ));
        drop(lock);
        assert!(path.exists());
        assert!(DownloadLock::acquire(&path).is_ok());
    }
    #[tokio::test]
    async fn complete_download_checks_hash_and_length() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, http) = downloader(vec![response(
            200,
            &[("content-length", "6"), ("etag", "\"v1\"")],
            &[b"abc", b"def"],
        )]);
        let result = downloader
            .download(
                TARGET,
                &expected(b"abcdef"),
                &path,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
        assert_eq!(result.resumed_from, 0);
        assert_eq!(http.requests.lock().unwrap()[0].range, None);
    }
    #[tokio::test]
    async fn wrong_hash_is_never_promoted() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, _) = downloader(vec![response(200, &[], &[b"bad"])]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"yes"), &path, &CancellationToken::new())
                .await,
            Err(DownloadError::Hash)
        ));
        assert!(clean_partial(&path));
    }
    #[tokio::test]
    async fn stream_overflow_is_rejected_before_extra_bytes_are_written() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, _) = downloader(vec![response(200, &[], &[b"abc", b"def"])]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"abcd"), &path, &CancellationToken::new())
                .await,
            Err(DownloadError::TooLarge)
        ));
        assert!(clean_partial(&path));
    }
    #[tokio::test]
    async fn early_eof_and_false_content_length_are_rejected() {
        for headers in [vec![], vec![("content-length", "3")]] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let (downloader, _) = downloader(vec![response(200, &headers, &[b"abc"])]);
            assert!(matches!(
                downloader
                    .download(
                        TARGET,
                        &expected(b"abcdef"),
                        &path,
                        &CancellationToken::new()
                    )
                    .await,
                Err(DownloadError::Length { .. })
            ));
            assert!(clean_partial(&path));
        }
    }
    #[tokio::test]
    async fn split_html_body_is_rejected_even_if_hash_matches() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, _) = downloader(vec![response(200, &[], &[b" \n<ht", b"ml>oops"])]);
        assert!(matches!(
            downloader
                .download(
                    TARGET,
                    &expected(b" \n<html>oops"),
                    &path,
                    &CancellationToken::new()
                )
                .await,
            Err(DownloadError::Html)
        ));
        assert!(clean_partial(&path));
    }
    #[tokio::test]
    async fn redirects_revalidate_every_hop_without_following_http() {
        for location in [
            "http://updates.example.test/a",
            "https://evil.example/a",
            "../app.exe",
            "/%2e%2e/a",
            "/a?redirect=1",
        ] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let (downloader, http) =
                downloader(vec![response(302, &[("location", location)], &[])]);
            assert!(matches!(
                downloader
                    .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
                    .await,
                Err(DownloadError::UrlPolicy(_))
            ));
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }
    #[tokio::test]
    async fn redirect_chain_is_bounded_and_relative_safe_redirect_works() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, http) = downloader(
            (0..4)
                .map(|_| response(302, &[("location", "/again")], &[]))
                .collect(),
        );
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
                .await,
            Err(DownloadError::RedirectLimit)
        ));
        assert_eq!(http.requests.lock().unwrap().len(), 4);
        let (downloader, http) = self::downloader(vec![
            response(307, &[("location", "/new")], &[]),
            response(200, &[], &[b"abc"]),
        ]);
        downloader
            .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            http.requests.lock().unwrap()[1].url.as_str(),
            "https://updates.example.test/new"
        );
    }
    #[tokio::test]
    async fn valid_resume_sends_range_and_if_range_and_rehashes_prefix() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abcdef");
        seed(&path, b"abc", &expected, "\"v1\"");
        let (downloader, http) = downloader(vec![response(
            206,
            &[
                ("etag", "\"v1\""),
                ("content-range", "bytes 3-5/6"),
                ("content-length", "3"),
            ],
            &[b"def"],
        )]);
        let result = downloader
            .download(TARGET, &expected, &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.resumed_from, 3);
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert_eq!(
            http.requests.lock().unwrap()[0].range,
            Some((3, "\"v1\"".into()))
        );
    }
    #[tokio::test]
    async fn changed_or_missing_resume_etag_invalidates_partial() {
        for headers in [
            vec![("etag", "\"v2\""), ("content-range", "bytes 3-5/6")],
            vec![("content-range", "bytes 3-5/6")],
            vec![("etag", "W/\"v1\""), ("content-range", "bytes 3-5/6")],
        ] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let expected = expected(b"abcdef");
            seed(&path, b"abc", &expected, "\"v1\"");
            let (downloader, _) = downloader(vec![response(206, &headers, &[b"def"])]);
            assert!(matches!(
                downloader
                    .download(TARGET, &expected, &path, &CancellationToken::new())
                    .await,
                Err(DownloadError::ChangedObject)
            ));
            assert!(clean_partial(&path));
        }
    }
    #[tokio::test]
    async fn incorrect_partial_start_end_total_or_unsolicited_range_fail() {
        for range in [
            "bytes 2-5/6",
            "bytes 3-4/6",
            "bytes 3-5/7",
            "bytes 3-5/*",
            "bytes 0-5/6",
            "bytes +3-5/6",
            "bytes 3-18446744073709551615/6",
        ] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let expected = expected(b"abcdef");
            seed(&path, b"abc", &expected, "\"v1\"");
            let (downloader, _) = downloader(vec![response(
                206,
                &[("etag", "\"v1\""), ("content-range", range)],
                &[b"def"],
            )]);
            assert!(
                matches!(
                    downloader
                        .download(TARGET, &expected, &path, &CancellationToken::new())
                        .await,
                    Err(DownloadError::Range)
                ),
                "{range}"
            );
            assert!(clean_partial(&path));
        }
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let (downloader, _) = downloader(vec![response(
            206,
            &[("content-range", "bytes 0-2/3")],
            &[b"abc"],
        )]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
                .await,
            Err(DownloadError::Range)
        ));
    }
    #[tokio::test]
    async fn resume_200_starts_over_instead_of_appending() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abcdef");
        seed(&path, b"BAD", &expected, "\"v1\"");
        let (downloader, _) = downloader(vec![response(200, &[("etag", "\"v2\"")], &[b"abcdef"])]);
        let result = downloader
            .download(TARGET, &expected, &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.resumed_from, 0);
        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
    }
    #[tokio::test]
    async fn corrupted_resume_prefix_cannot_pass_final_hash() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abcdef");
        seed(&path, b"BAD", &expected, "\"v1\"");
        let (downloader, _) = downloader(vec![response(
            206,
            &[("etag", "\"v1\""), ("content-range", "bytes 3-5/6")],
            &[b"def"],
        )]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected, &path, &CancellationToken::new())
                .await,
            Err(DownloadError::Hash)
        ));
        assert!(clean_partial(&path));
    }
    #[tokio::test]
    async fn oversized_partial_fails_without_network_request() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abc");
        seed(&path, b"abcd", &expected, "\"v1\"");
        let (downloader, http) = downloader(vec![]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected, &path, &CancellationToken::new())
                .await,
            Err(DownloadError::TooLarge)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
        assert!(clean_partial(&path));
    }
    #[tokio::test]
    async fn invalid_sidecar_or_weak_etag_forces_full_restart() {
        for sidecar in [
            b"invalid json".to_vec(),
            vec![b'x'; SIDECAR_CAP as usize + 1],
        ] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let expected = expected(b"abc");
            seed(&path, b"a", &expected, "\"v1\"");
            std::fs::write(DownloadPaths::new(&path).unwrap().state, sidecar).unwrap();
            let (downloader, http) = downloader(vec![response(200, &[], &[b"abc"])]);
            downloader
                .download(TARGET, &expected, &path, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(http.requests.lock().unwrap()[0].range, None);
        }
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abc");
        seed(&path, b"a", &expected, "W/\"v1\"");
        let (downloader, http) = downloader(vec![response(200, &[], &[b"abc"])]);
        downloader
            .download(TARGET, &expected, &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(http.requests.lock().unwrap()[0].range, None);
    }
    #[tokio::test]
    async fn cancellation_before_request_does_not_touch_existing_partial() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abc");
        seed(&path, b"a", &expected, "\"v1\"");
        let (downloader, http) = downloader(vec![]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            downloader.download(TARGET, &expected, &path, &cancel).await,
            Err(DownloadError::Cancelled)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read(DownloadPaths::new(&path).unwrap().part).unwrap(),
            b"a"
        );
    }
    #[tokio::test]
    async fn interruption_preserves_strongly_bound_bytes_then_resumes() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let expected = expected(b"abcdef");
        let mut interrupted = response(200, &[("etag", "\"v1\"")], &[]);
        interrupted.body = Box::pin(futures_util::stream::iter([
            Ok(Bytes::from_static(b"abc")),
            Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "simulated interruption",
            ))),
        ]));
        let (downloader, http) = downloader(vec![
            interrupted,
            response(
                206,
                &[("etag", "\"v1\""), ("content-range", "bytes 3-5/6")],
                &[b"def"],
            ),
        ]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected, &path, &CancellationToken::new())
                .await,
            Err(DownloadError::Io(_))
        ));
        assert_eq!(
            std::fs::read(DownloadPaths::new(&path).unwrap().part).unwrap(),
            b"abc"
        );
        let result = downloader
            .download(TARGET, &expected, &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.resumed_from, 3);
        assert_eq!(
            http.requests.lock().unwrap()[1].range,
            Some((3, "\"v1\"".into()))
        );
    }
    #[tokio::test]
    async fn body_cancellation_and_read_timeout_are_bounded() {
        let temp = private_tempdir();
        let path = temp.path().join("app.exe");
        let cancel = CancellationToken::new();
        let mut blocked = response(200, &[("etag", "\"v1\"")], &[]);
        let trigger = cancel.clone();
        blocked.body = Box::pin(futures_util::stream::poll_fn(move |_| {
            trigger.cancel();
            std::task::Poll::Pending
        }));
        let (downloader, _) = downloader(vec![blocked]);
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"abc"), &path, &cancel)
                .await,
            Err(DownloadError::Cancelled)
        ));
        let mut blocked = response(200, &[], &[]);
        blocked.body = Box::pin(futures_util::stream::pending());
        let (mut downloader, _) = self::downloader(vec![blocked]);
        downloader.policy.limits.read_timeout = Duration::from_millis(5);
        assert!(matches!(
            downloader
                .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
                .await,
            Err(DownloadError::Timeout)
        ));
    }
    #[tokio::test]
    async fn metadata_transport_enforces_cap_html_and_not_found() {
        let backend = backend(vec![
            response(200, &[], &[b"123", b"456"]),
            response(200, &[("content-type", "text/html")], &[b"x"]),
            response(404, &[], &[]),
        ]);
        let mut transport = HttpsTransport {
            policy: policy(),
            backend,
            cancel: CancellationToken::new(),
        };
        transport.policy.limits.max_response_bytes = 5;
        assert!(matches!(
            transport.fetch_bounded(Url::parse(TARGET).unwrap()).await,
            Err(DownloadError::TooLarge)
        ));
        assert!(matches!(
            transport.fetch_bounded(Url::parse(TARGET).unwrap()).await,
            Err(DownloadError::Html)
        ));
        let error = transport
            .fetch(Url::parse(TARGET).unwrap())
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), TransportErrorKind::FileNotFound);
    }
    #[tokio::test]
    async fn relative_destination_is_rejected_without_network() {
        let (downloader, http) = downloader(vec![]);
        assert!(matches!(
            downloader
                .download(
                    TARGET,
                    &expected(b"abc"),
                    Path::new("relative-cache/app.exe"),
                    &CancellationToken::new()
                )
                .await,
            Err(DownloadError::UnsafePath)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_cache_parent_is_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;
        let temp = private_tempdir();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("cache");
        symlink(&real, &link).unwrap();
        let (downloader, http) = downloader(vec![]);
        assert!(matches!(
            downloader
                .download(
                    TARGET,
                    &expected(b"abc"),
                    &link.join("app.exe"),
                    &CancellationToken::new()
                )
                .await,
            Err(DownloadError::UnsafePath)
        ));
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
        assert!(http.requests.lock().unwrap().is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn nonprivate_cache_and_writable_nonsticky_ancestor_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let temp = private_tempdir();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            DownloadPaths::new(&temp.path().join("app.exe")),
            Err(DownloadError::UnsafePath)
        ));
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let private = temp.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            DownloadPaths::new(&private.join("app.exe")),
            Err(DownloadError::UnsafePath)
        ));
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_partial_and_destination_are_rejected() {
        use std::os::unix::fs::symlink;
        for partial in [true, false] {
            let temp = private_tempdir();
            let path = temp.path().join("app.exe");
            let victim = temp.path().join("victim");
            std::fs::write(&victim, b"safe").unwrap();
            let link = if partial {
                DownloadPaths::new(&path).unwrap().part
            } else {
                path.clone()
            };
            symlink(&victim, link).unwrap();
            let (downloader, http) = downloader(vec![]);
            assert!(matches!(
                downloader
                    .download(TARGET, &expected(b"abc"), &path, &CancellationToken::new())
                    .await,
                Err(DownloadError::UnsafePath)
            ));
            assert_eq!(std::fs::read(&victim).unwrap(), b"safe");
            assert!(http.requests.lock().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn pause_token_preserves_valid_partial_and_a_fresh_token_resumes() {
        let temp = private_tempdir();
        let path = temp.path().join("model-pack.zip");
        let expected = expected(b"abcdef");
        let pause = CancellationToken::new();
        let trigger = pause.clone();
        let mut sent = false;
        let mut interrupted = response(200, &[("etag", "\"v1\"")], &[]);
        interrupted.body = Box::pin(futures_util::stream::poll_fn(move |_| {
            if !sent {
                sent = true;
                std::task::Poll::Ready(Some(Ok(Bytes::from_static(b"abc"))))
            } else {
                trigger.cancel();
                std::task::Poll::Pending
            }
        }));
        let (downloader, http) = downloader(vec![
            interrupted,
            response(
                206,
                &[("etag", "\"v1\""), ("content-range", "bytes 3-5/6")],
                &[b"def"],
            ),
        ]);
        assert!(matches!(
            downloader.download(TARGET, &expected, &path, &pause).await,
            Err(DownloadError::Cancelled)
        ));
        let paths = DownloadPaths::new(&path).unwrap();
        assert_eq!(std::fs::read(&paths.part).unwrap(), b"abc");
        assert!(paths.state.exists());
        assert!(!path.exists());
        let result = downloader
            .download(TARGET, &expected, &path, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.resumed_from, 3);
        assert_eq!(
            http.requests.lock().unwrap()[1].range,
            Some((3, "\"v1\"".into()))
        );
        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
    }
}
