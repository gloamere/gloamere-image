use crate::config::{MAX_METADATA_VERSION, MAX_ROOT_CHAIN};
use crate::download::{
    Downloader, ExpectedTarget, HttpsPolicy, HttpsTransport, TransportLimits, VerifiedDownload,
};
use crate::state::{ReleaseFloor, StoredMetadata, check_floor, floor, validate_root_chain};
use crate::{
    APP_PACKAGE_LIMIT, CancellationToken, Error, LockedState, METADATA_LIMIT, PRODUCT,
    ProductionConfig, Result,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tough::schema::{Metafile, Role, Root, Signed, Snapshot, Targets, Timestamp};
use tough::{
    Bytes, Repository, RepositoryLoader, Transport, TransportError, TransportErrorKind,
    TransportStream,
};
use url::Url;

#[derive(Clone, Debug)]
pub struct Compatibility {
    pub installed_release_id: u64,
    pub updater_protocol: u32,
    pub launcher_protocol: u32,
    pub settings_schema: u32,
    pub ai_worker_protocol: u32,
    /// Stable local random installation identity. Never sent to the repository.
    pub installation_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseMetadata {
    pub product: String,
    pub kind: String,
    pub version: String,
    pub release_id: u64,
    pub channel: String,
    pub platform: String,
    pub install_scope: String,
    pub min_updater_protocol: u32,
    pub min_launcher_protocol: u32,
    pub settings_compat: Vec<u32>,
    pub ai_worker_protocol: u32,
    pub rollout_percent: u8,
    pub rollout_seed: String,
    #[serde(default)]
    pub notes_path: Option<String>,
}

/// Opaque trust-verified descriptor. It cannot be constructed/deserialized from
/// an unsigned remote manifest. It contains no executable command or install path.
#[derive(Clone, Debug)]
pub struct VerifiedTarget {
    release: ReleaseMetadata,
    target_name: String,
    url: String,
    length: u64,
    sha256: [u8; 32],
    configuration_binding: String,
    valid_until: i64,
    selected_at: i64,
    release_identity: String,
    metadata_identity: BTreeMap<String, crate::MetadataFloor>,
}
impl VerifiedTarget {
    pub fn release(&self) -> &ReleaseMetadata {
        &self.release
    }
    pub fn target_name(&self) -> &str {
        &self.target_name
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn valid_until_unix_seconds(&self) -> i64 {
        self.valid_until
    }
    pub(crate) fn ensure_fresh_at(&self, now: i64) -> Result<()> {
        if now < self.selected_at {
            return Err(Error::Rollback(
                "wall clock moved backward after target selection",
            ));
        }
        if now >= self.valid_until {
            return Err(Error::Policy("trusted target metadata expired"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct TrustedRepository {
    pub(crate) repository: Repository,
    pub(crate) configuration_binding: String,
    pub(crate) valid_until: i64,
    pub(crate) metadata_identity: BTreeMap<String, crate::MetadataFloor>,
}

impl TrustedRepository {
    /// Select application updates only. A model target/approved flag never
    /// confers model quality or authorizes a model runtime installation.
    pub fn select_app(
        &self,
        config: &ProductionConfig,
        state: &mut LockedState,
        compatibility: &Compatibility,
    ) -> Result<Option<VerifiedTarget>> {
        if self.configuration_binding != config.binding()
            || state.state.configuration_binding != config.binding()
        {
            return Err(Error::State("repository/state/configuration mismatch"));
        }
        if self.metadata_identity != state.state.highest_metadata {
            return Err(Error::Rollback(
                "repository snapshot was superseded; select from the latest verified metadata",
            ));
        }
        let now = jiff::Timestamp::now().as_second();
        state.check_clock(now)?;
        if now >= self.valid_until {
            return Err(Error::Policy("verified metadata expired before selection"));
        }
        if compatibility.installation_id.is_empty() || compatibility.installation_id.len() > 128 {
            return Err(Error::Policy(
                "bounded stable installation identity required for rollout",
            ));
        }
        let mut candidates = BTreeMap::new();
        for (name, target) in &self.repository.targets().signed.targets {
            validate_relative_path(name.raw())?;
            let kind = target.custom.get("kind").and_then(Value::as_str);
            if kind != Some("app") {
                continue;
            }
            let release: ReleaseMetadata =
                serde_json::from_value(serde_json::to_value(&target.custom)?)?;
            validate_release(&release, name.raw(), target.length)?;
            if release.product != PRODUCT
                || release.platform != config.platform
                || release.channel != config.channel
            {
                continue;
            }
            let hash: [u8; 32] = target
                .hashes
                .sha256
                .as_ref()
                .try_into()
                .map_err(|_| Error::Policy("target SHA-256 must be 32 bytes"))?;
            if candidates
                .insert(release.release_id, (name, target, release, hash))
                .is_some()
            {
                return Err(Error::Policy("duplicate app release sequence"));
            }
        }
        let Some((_, (name, target, release, hash))) = candidates.last_key_value() else {
            return Ok(None);
        };
        // Persist the highest authenticated release even when compatibility,
        // rollout, installed version or quarantine makes it ineligible today.
        let identity = crate::state::signed_target_identity(name.raw(), target)?;
        let scope = format!("{PRODUCT}/{}/{}/app", config.platform, config.channel);
        if let Some(old) = state.state.releases.get(&scope) {
            if release.release_id < old.sequence {
                return Err(Error::Rollback("older signed release sequence"));
            }
            if release.release_id == old.sequence && identity != old.identity {
                return Err(Error::Rollback(
                    "same release sequence identifies different content",
                ));
            }
        }
        let mut next = state.state.clone();
        next.releases.insert(
            scope,
            ReleaseFloor {
                sequence: release.release_id,
                identity: identity.clone(),
            },
        );
        next.last_observed_unix_seconds = now;
        state.commit(next)?;
        if release.release_id <= compatibility.installed_release_id
            || state
                .state
                .quarantined_app_sequences
                .contains(&release.release_id)
        {
            return Ok(None);
        }
        if compatibility.updater_protocol < release.min_updater_protocol
            || compatibility.launcher_protocol < release.min_launcher_protocol
            || !release
                .settings_compat
                .contains(&compatibility.settings_schema)
            || compatibility.ai_worker_protocol != release.ai_worker_protocol
        {
            return Err(Error::Policy(
                "application/launcher/settings/worker compatibility mismatch",
            ));
        }
        let cohort = Sha256::digest(
            format!(
                "{}\0{}",
                release.rollout_seed, compatibility.installation_id
            )
            .as_bytes(),
        );
        let bucket = u32::from_be_bytes(cohort[..4].try_into().expect("4 bytes")) % 100;
        if bucket >= u32::from(release.rollout_percent) {
            return Ok(None);
        }
        // TUF consistent-snapshot target URL: digest prefix on the basename.
        let (parent, filename) = name
            .raw()
            .rsplit_once('/')
            .ok_or(Error::Policy("target path"))?;
        let path = format!("{parent}/{}.{}", hex::encode(hash), filename);
        let url = config
            .targets_url
            .join(&path)
            .map_err(|_| Error::Policy("target URL"))?;
        Ok(Some(VerifiedTarget {
            release: release.clone(),
            target_name: name.raw().into(),
            url: url.to_string(),
            length: target.length,
            sha256: *hash,
            configuration_binding: config.binding(),
            valid_until: self.valid_until,
            selected_at: now,
            release_identity: identity,
            metadata_identity: state.state.highest_metadata.clone(),
        }))
    }
}

fn validate_release(release: &ReleaseMetadata, name: &str, length: u64) -> Result<()> {
    if release.kind != "app"
        || release.release_id == 0
        || release.release_id > MAX_METADATA_VERSION
        || release.install_scope != "current-user"
        || release.min_updater_protocol == 0
        || release.min_launcher_protocol == 0
        || release.ai_worker_protocol == 0
        || release.settings_compat.is_empty()
        || release.settings_compat.len() > 16
        || release.settings_compat.contains(&0)
        || release.rollout_percent > 100
        || release.rollout_seed.is_empty()
        || release.rollout_seed.len() > 128
        || release.version.is_empty()
        || release.version.len() > 64
        || !release
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
        || length == 0
        || length > APP_PACKAGE_LIMIT
    {
        return Err(Error::Policy(
            "invalid signed app release fields or package length",
        ));
    }
    if name
        != format!(
            "releases/{}/{}/{}/Setup.exe",
            release.platform, release.channel, release.release_id
        )
    {
        return Err(Error::Policy("target path disagrees with release identity"));
    }
    if let Some(notes) = &release.notes_path {
        validate_relative_path(notes)?;
        if !notes.starts_with("notes/") || !notes.ends_with(".txt") {
            return Err(Error::Policy("notes must be a fixed text target"));
        }
    }
    Ok(())
}

pub(crate) fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 512
        || !path.is_ascii()
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
        || path.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.ends_with('.')
                || part.ends_with(' ')
        })
    {
        return Err(Error::Policy("unsafe metadata target path"));
    }
    Ok(())
}

fn policy(config: &ProductionConfig) -> Result<HttpsPolicy> {
    HttpsPolicy::new(
        vec![config.origin.clone()],
        TransportLimits {
            max_target_bytes: APP_PACKAGE_LIMIT,
            ..Default::default()
        },
    )
    .map_err(|e| Error::Download(e.to_string()))
}

pub async fn refresh(
    config: &ProductionConfig,
    state: &mut LockedState,
    cancel: &CancellationToken,
) -> Result<TrustedRepository> {
    let transport = HttpsTransport::new(policy(config)?, cancel.clone())
        .map_err(|e| Error::Download(e.to_string()))?;
    refresh_with_transport(config, state, cancel, transport).await
}

pub fn blocking_check(
    config: &ProductionConfig,
    state: &mut LockedState,
    compatibility: &Compatibility,
    cancel: &CancellationToken,
) -> Result<Option<VerifiedTarget>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let repository = runtime.block_on(refresh(config, state, cancel))?;
    repository.select_app(config, state, compatibility)
}

/// Last authorization check under the caller's install lock, immediately before
/// the platform rehashes and verifies the executable publisher. Never executes.
pub fn revalidate_for_execution(
    config: &ProductionConfig,
    target: &VerifiedTarget,
    state: &mut LockedState,
) -> Result<()> {
    if target.configuration_binding != config.binding()
        || state.state.configuration_binding != config.binding()
    {
        return Err(Error::State("execution configuration binding mismatch"));
    }
    let now = jiff::Timestamp::now().as_second();
    state.check_clock(now)?;
    if now >= target.valid_until {
        return Err(Error::Policy("trusted metadata expired before execution"));
    }
    if target.metadata_identity != state.state.highest_metadata {
        return Err(Error::Rollback(
            "metadata changed after target selection; select again",
        ));
    }
    let scope = format!("{PRODUCT}/{}/{}/app", config.platform, config.channel);
    let release = state
        .state
        .releases
        .get(&scope)
        .ok_or(Error::State("selected release floor missing"))?;
    if release.sequence != target.release.release_id || release.identity != target.release_identity
    {
        return Err(Error::Rollback(
            "target no longer matches highest trusted release",
        ));
    }
    if state
        .state
        .quarantined_app_sequences
        .contains(&target.release.release_id)
    {
        return Err(Error::Policy("failed release is quarantined"));
    }
    let mut next = state.state.clone();
    next.last_observed_unix_seconds = now;
    state.commit(next)
}

pub fn blocking_download(
    config: &ProductionConfig,
    target: &VerifiedTarget,
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<VerifiedDownload> {
    if target.configuration_binding != config.binding() {
        return Err(Error::State("target/configuration mismatch"));
    }
    target.ensure_fresh_at(jiff::Timestamp::now().as_second())?;
    let downloader =
        Downloader::new(policy(config)?).map_err(|e| Error::Download(e.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime
        .block_on(downloader.download(
            &target.url,
            &ExpectedTarget {
                length: target.length,
                sha256: target.sha256,
            },
            destination,
            cancel,
        ))
        .map_err(|e| Error::Download(e.to_string()))?;
    target.ensure_fresh_at(jiff::Timestamp::now().as_second())?;
    Ok(result)
}

pub(crate) async fn refresh_with_transport<T: Transport + Clone + 'static>(
    config: &ProductionConfig,
    state: &mut LockedState,
    cancel: &CancellationToken,
    transport: T,
) -> Result<TrustedRepository> {
    if state.state.configuration_binding != config.binding() {
        return Err(Error::State("trust binding mismatch"));
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let now = jiff::Timestamp::now().as_second();
    state.check_clock(now)?;
    let roots = validate_root_chain(&state.state, config)?;
    let last_root = roots.last().expect("anchor present");
    let root_bytes = serde_json::to_vec(last_root)?;
    let work = tempfile::Builder::new()
        .prefix(".metadata-")
        .tempdir_in(state.directory())?;
    // tough writes its datastore non-atomically. It is only scratch. Our durable,
    // locked state commits verified evidence atomically, including partial errors.
    for (role, data) in &state.state.metadata {
        std::fs::write(
            work.path().join(format!("{role}.json")),
            serde_json::to_vec(&data.envelope)?,
        )?;
    }
    let recorder = RecordingTransport {
        inner: transport,
        roots: Arc::new(Mutex::new(Vec::new())),
        fatal: Arc::new(Mutex::new(None)),
        requests: Arc::new(Mutex::new(0)),
        metadata_base: config.metadata_url.clone(),
    };
    let loader = RepositoryLoader::new(
        &root_bytes,
        config.metadata_url.clone(),
        config.targets_url.clone(),
    )
    .transport(recorder.clone())
    .datastore(work.path())
    .limits(tough::Limits {
        max_root_size: METADATA_LIMIT,
        max_targets_size: METADATA_LIMIT,
        max_timestamp_size: METADATA_LIMIT,
        max_snapshot_size: METADATA_LIMIT,
        max_root_updates: 16,
    });
    let loaded = tokio::select! {
        _ = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(90), loader.load()) => match result {
            Ok(result) => result.map_err(|e| Error::Tuf(e.to_string())),
            Err(_) => Err(Error::Policy("metadata refresh deadline exceeded")),
        }
    };
    // Preserve authenticated floors even when later metadata is unavailable,
    // invalid, expired or incompatible. Never retain unverified downloaded JSON.
    ingest_verified_progress(config, state, work.path(), &recorder, now)?;
    if let Some(error) = recorder
        .fatal
        .lock()
        .map_err(|_| Error::State("transport lock poisoned"))?
        .as_ref()
    {
        return Err(Error::Tuf(error.clone()));
    }
    let repository = loaded?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    validate_profile(&repository, now)?;
    let valid_until = [
        repository.root().signed.expires.as_second(),
        repository.timestamp().signed.expires.as_second(),
        repository.snapshot().signed.expires.as_second(),
        repository.targets().signed.expires.as_second(),
    ]
    .into_iter()
    .min()
    .expect("4 expiries");
    Ok(TrustedRepository {
        repository,
        configuration_binding: config.binding(),
        valid_until,
        metadata_identity: state.state.highest_metadata.clone(),
    })
}

fn validate_profile(repository: &Repository, now: i64) -> Result<()> {
    let root = &repository.root().signed;
    if !root.consistent_snapshot {
        return Err(Error::Policy("consistent snapshots required"));
    }
    role_time(root, &root._extra, now, 366 * 86400)?;
    let timestamp = &repository.timestamp().signed;
    role_time(timestamp, &timestamp._extra, now, 2 * 86400)?;
    let snapshot = &repository.snapshot().signed;
    role_time(snapshot, &snapshot._extra, now, 31 * 86400)?;
    let targets = &repository.targets().signed;
    role_time(targets, &targets._extra, now, 31 * 86400)?;
    if targets.delegations.is_some() || targets.targets.len() > 1024 || snapshot.meta.len() != 1 {
        return Err(Error::Policy("only bounded top-level targets supported"));
    }
    binding(
        timestamp
            .meta
            .get("snapshot.json")
            .ok_or(Error::Policy("missing snapshot binding"))?,
    )?;
    binding(
        snapshot
            .meta
            .get("targets.json")
            .ok_or(Error::Policy("missing targets binding"))?,
    )?;
    for (name, target) in &targets.targets {
        validate_relative_path(name.raw())?;
        if target.length == 0
            || target.length > 1024 * 1024 * 1024
            || target.hashes.sha256.as_ref().len() != 32
        {
            return Err(Error::Policy(
                "target length/hash outside repository profile",
            ));
        }
    }
    Ok(())
}

fn role_time<T: Role>(
    role: &T,
    extra: &std::collections::HashMap<String, Value>,
    now: i64,
    maximum_lifetime: i64,
) -> Result<()> {
    let issued = extra
        .get("issued")
        .and_then(Value::as_str)
        .ok_or(Error::Policy("signed issued timestamp required"))?;
    let issued: jiff::Timestamp = issued
        .parse()
        .map_err(|_| Error::Policy("invalid issued timestamp"))?;
    let expiry = role.expires().as_second();
    if issued.as_second() > now
        || expiry <= now
        || expiry <= issued.as_second()
        || expiry
            .checked_sub(issued.as_second())
            .is_none_or(|lifetime| lifetime > maximum_lifetime)
    {
        return Err(Error::Policy(
            "future, expired or excessive metadata validity window",
        ));
    }
    floor(role)?;
    Ok(())
}

fn binding(meta: &Metafile) -> Result<()> {
    if meta
        .length
        .is_none_or(|length| length == 0 || length > METADATA_LIMIT)
        || meta
            .hashes
            .as_ref()
            .is_none_or(|hashes| hashes.sha256.as_ref().len() != 32)
        || meta.version.get() > MAX_METADATA_VERSION
    {
        return Err(Error::Policy(
            "metadata binding requires bounded length, SHA-256 and version",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct RecordingTransport<T> {
    inner: T,
    roots: Arc<Mutex<Vec<Value>>>,
    fatal: Arc<Mutex<Option<String>>>,
    requests: Arc<Mutex<u32>>,
    metadata_base: Url,
}
#[tough::async_trait]
impl<T: Transport + Clone + 'static> Transport for RecordingTransport<T> {
    async fn fetch(&self, url: Url) -> std::result::Result<TransportStream, TransportError> {
        let fail = |reason: &str| {
            if let Ok(mut saved) = self.fatal.lock() {
                *saved = Some(reason.to_owned());
            }
            TransportError::new_with_cause(
                TransportErrorKind::Other,
                &url,
                std::io::Error::other(reason.to_owned()),
            )
        };
        let path = url
            .path()
            .strip_prefix(self.metadata_base.path())
            .ok_or_else(|| fail("metadata path outside repository"))?;
        let recognized = path == "timestamp.json"
            || [".root.json", ".snapshot.json", ".targets.json"]
                .iter()
                .any(|suffix| {
                    path.strip_suffix(suffix).is_some_and(|version| {
                        !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
                    })
                });
        if url.origin() != self.metadata_base.origin() || !recognized {
            return Err(fail("delegated or arbitrary metadata URL rejected"));
        }
        {
            let mut count = self.requests.lock().map_err(|_| fail("request lock"))?;
            *count += 1;
            if *count > 24 {
                return Err(fail("metadata request count limit"));
            }
        }
        let mut stream = self.inner.fetch(url.clone()).await.inspect_err(|error| {
            if error.kind() != TransportErrorKind::FileNotFound
                && let Ok(mut fatal) = self.fatal.lock()
            {
                *fatal = Some(error.to_string());
            }
        })?;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.inspect_err(|e| {
                if let Ok(mut fatal) = self.fatal.lock() {
                    *fatal = Some(e.to_string());
                }
            })?;
            if (bytes.len() as u64)
                .checked_add(chunk.len() as u64)
                .is_none_or(|total| total > METADATA_LIMIT)
            {
                return Err(fail("metadata body too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if path.ends_with(".root.json")
            && let Ok(value) = serde_json::from_slice(&bytes)
        {
            self.roots
                .lock()
                .map_err(|_| fail("root capture lock"))?
                .push(value);
        }
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from(
            bytes,
        ))])))
    }
}

fn ingest_verified_progress<T: Transport + Clone + 'static>(
    config: &ProductionConfig,
    state: &mut LockedState,
    work: &Path,
    recorder: &RecordingTransport<T>,
    now: i64,
) -> Result<()> {
    let mut next = state.state.clone();
    next.last_observed_unix_seconds = now;
    let roots = validate_root_chain(&next, config)?;
    let mut current = roots.last().expect("anchor present").clone();
    let captured = recorder
        .roots
        .lock()
        .map_err(|_| Error::State("root capture lock poisoned"))?;
    for value in captured.iter() {
        let candidate: Signed<Root> = match serde_json::from_value(value.clone()) {
            Ok(root) => root,
            Err(_) => break,
        };
        if current.signed.version.get().checked_add(1) != Some(candidate.signed.version.get())
            || candidate.signed.version.get() > MAX_METADATA_VERSION
            || !candidate.signed.consistent_snapshot
            || current.signed.verify_role(&candidate).is_err()
            || candidate.signed.verify_role(&candidate).is_err()
        {
            break;
        }
        if next.root_chain.len() >= MAX_ROOT_CHAIN {
            return Err(Error::Policy(
                "root chain exhausted; reviewed anchor refresh required",
            ));
        }
        next.root_chain.push(value.clone());
        next.highest_metadata
            .insert("root".into(), floor(&candidate.signed)?);
        current = candidate;
    }
    drop(captured);
    let mut rejection = None;
    for name in ["timestamp", "snapshot", "targets"] {
        let path = work.join(format!("{name}.json"));
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path)?;
        if bytes.len() as u64 > METADATA_LIMIT {
            return Err(Error::State("oversized scratch metadata"));
        }
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let verified = match name {
            "timestamp" => authenticated_floor::<Timestamp>(&current, &value),
            "snapshot" => authenticated_floor::<Snapshot>(&current, &value),
            "targets" => authenticated_floor::<Targets>(&current, &value),
            _ => unreachable!(),
        };
        if let Ok(verified) = verified {
            if let Err(error) = check_floor(next.highest_metadata.get(name), &verified) {
                rejection = Some(error);
                break;
            }
            next.highest_metadata.insert(name.into(), verified);
            next.metadata.insert(
                name.into(),
                StoredMetadata {
                    root_version: current.signed.version.get(),
                    envelope: value,
                },
            );
        }
    }
    state.commit(next)?;
    if let Some(error) = rejection {
        return Err(error);
    }
    Ok(())
}
fn authenticated_floor<T: Role + serde::de::DeserializeOwned>(
    root: &Signed<Root>,
    value: &Value,
) -> Result<crate::MetadataFloor> {
    let signed: Signed<T> = serde_json::from_value(value.clone())?;
    root.signed
        .verify_role(&signed)
        .map_err(|e| Error::Tuf(e.to_string()))?;
    floor(&signed.signed)
}
