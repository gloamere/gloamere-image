//! Independent signed model packages. Authenticity does not grant model quality,
//! consent, new graph/native-code allowlists, extraction or activation authority.
use crate::config::MAX_METADATA_VERSION;
use crate::download::{Downloader, ExpectedTarget, HttpsPolicy, TransportLimits, VerifiedDownload};
use crate::metadata::validate_relative_path;
use crate::state::ReleaseFloor;
use crate::{
    CancellationToken, Error, LockedState, MODEL_PACKAGE_LIMIT, MODEL_UNPACKED_LIMIT, PRODUCT,
    ProductionConfig, Result, TrustedRepository,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Construct exclusively from reviewed, compiled model policy and local state.
/// Never populate the allowed identity from downloaded JSON. Consent is separate
/// and must be checked by the caller before invoking network work.
#[derive(Clone, Debug, Serialize)]
pub struct ModelCompatibility {
    pub installed_release_id: u64,
    pub app_protocol: u32,
    pub installation_id: String,
    pub model_id: String,
    pub model_version: String,
    pub pack_schema: u32,
    pub preprocess_version: String,
    pub worker_protocol: u32,
    pub runtime_version: String,
    pub source_revision: String,
    pub pack_fingerprint: String,
    pub license_policy: String,
    pub license_files: Vec<String>,
    pub manifest_sha256: String,
    pub unpacked_length: u64,
}
impl ModelCompatibility {
    fn policy_binding(&self) -> Result<String> {
        let mut policy = self.clone();
        // Local installed sequence/cohort are not a content allowlist. Their
        // changes do not authorize a new graph, native library or quality level.
        policy.installed_release_id = 0;
        policy.installation_id.clear();
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&policy)?)))
    }
    fn validate(&self) -> Result<()> {
        validate_model_id(&self.model_id)?;
        validate_hash(&self.model_version)?;
        validate_hash(&self.manifest_sha256)?;
        if self.app_protocol == 0
            || self.pack_schema == 0
            || self.worker_protocol == 0
            || self.preprocess_version.is_empty()
            || self.preprocess_version.len() > 128
            || self.runtime_version.is_empty()
            || self.runtime_version.len() > 64
            || self.source_revision.is_empty()
            || self.source_revision.len() > 128
            || self.pack_fingerprint.is_empty()
            || self.pack_fingerprint.len() > 1024
            || self.license_policy.is_empty()
            || self.license_policy.len() > 128
            || self.license_files.is_empty()
            || self.license_files.len() > 64
            || self.installation_id.is_empty()
            || self.installation_id.len() > 128
            || self.unpacked_length == 0
            || self.unpacked_length > MODEL_UNPACKED_LIMIT
        {
            return Err(Error::Policy("invalid compiled model compatibility policy"));
        }
        for name in &self.license_files {
            validate_model_id(name)?;
        }
        Ok(())
    }
}

/// Exact signed model profile. There is deliberately no approved/stable/quality
/// field: qualification remains compiled application policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelReleaseMetadata {
    pub product: String,
    pub kind: String,
    pub model_id: String,
    pub model_version: String,
    pub release_id: u64,
    pub channel: String,
    pub platform: String,
    pub install_scope: String,
    pub min_app_protocol: u32,
    pub pack_schema: u32,
    pub preprocess_version: String,
    pub worker_protocol: u32,
    pub runtime_version: String,
    pub source_revision: String,
    pub pack_fingerprint: String,
    pub license_policy: String,
    pub license_files: Vec<String>,
    pub manifest_sha256: String,
    pub unpacked_length: u64,
    pub rollout_percent: u8,
    pub rollout_seed: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedModelTarget {
    release: ModelReleaseMetadata,
    target_name: String,
    url: String,
    length: u64,
    sha256: [u8; 32],
    configuration_binding: String,
    compiled_policy_binding: String,
    metadata_identity: BTreeMap<String, crate::MetadataFloor>,
    release_identity: String,
    selected_at: i64,
    valid_until: i64,
}
impl VerifiedModelTarget {
    pub fn release(&self) -> &ModelReleaseMetadata {
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
    fn ensure_fresh_at(&self, now: i64) -> Result<()> {
        if now < self.selected_at {
            return Err(Error::Rollback(
                "clock moved backward after model selection",
            ));
        }
        if now >= self.valid_until {
            return Err(Error::Policy("model target metadata expired"));
        }
        Ok(())
    }
}

impl TrustedRepository {
    pub fn select_model(
        &self,
        config: &ProductionConfig,
        state: &mut LockedState,
        compatible: &ModelCompatibility,
    ) -> Result<Option<VerifiedModelTarget>> {
        compatible.validate()?;
        if self.configuration_binding != config.binding()
            || state.state.configuration_binding != config.binding()
        {
            return Err(Error::State(
                "model repository/state/configuration mismatch",
            ));
        }
        if self.metadata_identity != state.state.highest_metadata {
            return Err(Error::Rollback("model repository snapshot was superseded"));
        }
        let now = jiff::Timestamp::now().as_second();
        state.check_clock(now)?;
        if now >= self.valid_until {
            return Err(Error::Policy("model metadata expired before selection"));
        }
        let mut candidates = BTreeMap::new();
        for (name, target) in &self.repository.targets().signed.targets {
            if target.custom.get("kind").and_then(Value::as_str) != Some("model") {
                continue;
            }
            let release: ModelReleaseMetadata =
                serde_json::from_value(serde_json::to_value(&target.custom)?)?;
            validate_release(&release, name.raw(), target.length)?;
            if release.product != PRODUCT
                || release.platform != config.platform
                || release.channel != config.channel
                || release.model_id != compatible.model_id
            {
                continue;
            }
            let hash: [u8; 32] = target
                .hashes
                .sha256
                .as_ref()
                .try_into()
                .map_err(|_| Error::Policy("model package SHA-256 must be 32 bytes"))?;
            if candidates
                .insert(release.release_id, (name, target, release, hash))
                .is_some()
            {
                return Err(Error::Policy("duplicate model release sequence"));
            }
        }
        let Some((_, (name, target, release, hash))) = candidates.last_key_value() else {
            return Ok(None);
        };
        let identity = crate::state::signed_target_identity(name.raw(), target)?;
        let scope = model_scope(config, &compatible.model_id);
        if let Some(old) = state.state.releases.get(&scope) {
            if release.release_id < old.sequence {
                return Err(Error::Rollback("older signed model release"));
            }
            if release.release_id == old.sequence && identity != old.identity {
                return Err(Error::Rollback("model release sequence changed content"));
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
        // Authenticate and remember the sequence before applying compatibility,
        // but never hand out activation authority for an unknown compiled profile.
        ensure_compatible(release, compatible)?;
        if release.release_id <= compatible.installed_release_id
            || is_quarantined(state, &release.model_id, release.release_id)
        {
            return Ok(None);
        }
        let cohort = Sha256::digest(format!(
            "{}\0{}\0{}",
            release.model_id, release.rollout_seed, compatible.installation_id
        ));
        let bucket = u32::from_be_bytes(cohort[..4].try_into().expect("4 bytes")) % 100;
        if bucket >= u32::from(release.rollout_percent) {
            return Ok(None);
        }
        let (parent, filename) = name
            .raw()
            .rsplit_once('/')
            .ok_or(Error::Policy("model target path"))?;
        let url = config
            .targets_url
            .join(&format!("{parent}/{}.{}", hex::encode(hash), filename))
            .map_err(|_| Error::Policy("model target URL"))?;
        Ok(Some(VerifiedModelTarget {
            release: release.clone(),
            target_name: name.raw().into(),
            url: url.to_string(),
            length: target.length,
            sha256: *hash,
            configuration_binding: config.binding(),
            compiled_policy_binding: compatible.policy_binding()?,
            metadata_identity: state.state.highest_metadata.clone(),
            release_identity: identity,
            selected_at: now,
            valid_until: self.valid_until,
        }))
    }
}

pub fn blocking_check_model(
    config: &ProductionConfig,
    state: &mut LockedState,
    compatible: &ModelCompatibility,
    cancel: &CancellationToken,
) -> Result<Option<VerifiedModelTarget>> {
    compatible.validate()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime
        .block_on(crate::refresh(config, state, cancel))?
        .select_model(config, state, compatible)
}

/// Downloads an authenticated known-profile ZIP only. The caller checks separate
/// model-update consent and must run the pinned importer/self-test before activation.
pub fn blocking_download_model(
    config: &ProductionConfig,
    target: &VerifiedModelTarget,
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<VerifiedDownload> {
    if target.configuration_binding != config.binding() {
        return Err(Error::State("model target/configuration mismatch"));
    }
    target.ensure_fresh_at(jiff::Timestamp::now().as_second())?;
    let policy = HttpsPolicy::new(
        vec![config.origin.clone()],
        TransportLimits {
            max_target_bytes: MODEL_PACKAGE_LIMIT,
            ..Default::default()
        },
    )
    .map_err(|e| Error::Download(e.to_string()))?;
    let downloader = Downloader::new(policy).map_err(|e| Error::Download(e.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let output = runtime
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
    Ok(output)
}

/// Invoke under the model activation lock after exact extraction checks and before
/// replacing the active-pack pointer. No executable/graph is loaded by this call.
pub fn revalidate_model_for_activation(
    config: &ProductionConfig,
    target: &VerifiedModelTarget,
    state: &mut LockedState,
    compatible: &ModelCompatibility,
) -> Result<()> {
    compatible.validate()?;
    if target.configuration_binding != config.binding()
        || state.state.configuration_binding != config.binding()
        || target.compiled_policy_binding != compatible.policy_binding()?
    {
        return Err(Error::State("model activation policy binding mismatch"));
    }
    let now = jiff::Timestamp::now().as_second();
    state.check_clock(now)?;
    target.ensure_fresh_at(now)?;
    if target.metadata_identity != state.state.highest_metadata {
        return Err(Error::Rollback("model metadata changed after selection"));
    }
    ensure_compatible(&target.release, compatible)?;
    let release = state
        .state
        .releases
        .get(&model_scope(config, &target.release.model_id))
        .ok_or(Error::State("model release floor missing"))?;
    if release.sequence != target.release.release_id || release.identity != target.release_identity
    {
        return Err(Error::Rollback(
            "model target no longer matches highest release",
        ));
    }
    if is_quarantined(state, &target.release.model_id, target.release.release_id) {
        return Err(Error::Policy("failed model release is quarantined"));
    }
    if target.release.release_id <= compatible.installed_release_id {
        return Err(Error::Rollback(
            "model activation cannot downgrade or reinstall an installed release",
        ));
    }
    let mut next = state.state.clone();
    next.last_observed_unix_seconds = now;
    state.commit(next)
}

fn model_scope(config: &ProductionConfig, model_id: &str) -> String {
    format!(
        "{PRODUCT}/{}/{}/model/{model_id}",
        config.platform, config.channel
    )
}
fn is_quarantined(state: &LockedState, model_id: &str, sequence: u64) -> bool {
    state
        .state
        .quarantined_model_sequences
        .get(model_id)
        .is_some_and(|set| set.contains(&sequence))
}
fn ensure_compatible(release: &ModelReleaseMetadata, policy: &ModelCompatibility) -> Result<()> {
    if policy.app_protocol < release.min_app_protocol
        || release.model_id != policy.model_id
        || release.model_version != policy.model_version
        || release.pack_schema != policy.pack_schema
        || release.preprocess_version != policy.preprocess_version
        || release.worker_protocol != policy.worker_protocol
        || release.runtime_version != policy.runtime_version
        || release.source_revision != policy.source_revision
        || release.pack_fingerprint != policy.pack_fingerprint
        || release.license_policy != policy.license_policy
        || release.license_files != policy.license_files
        || release.manifest_sha256 != policy.manifest_sha256
        || release.unpacked_length != policy.unpacked_length
    {
        return Err(Error::Policy(
            "signed model package does not match compiled model/runtime/preprocessing/license allowlist",
        ));
    }
    Ok(())
}
fn validate_release(release: &ModelReleaseMetadata, path: &str, length: u64) -> Result<()> {
    validate_model_id(&release.model_id)?;
    validate_hash(&release.model_version)?;
    validate_hash(&release.manifest_sha256)?;
    validate_relative_path(path)?;
    if release.kind != "model"
        || release.release_id == 0
        || release.release_id > MAX_METADATA_VERSION
        || release.install_scope != "current-user"
        || release.min_app_protocol == 0
        || release.pack_schema == 0
        || release.worker_protocol == 0
        || release.preprocess_version.is_empty()
        || release.preprocess_version.len() > 128
        || release.runtime_version.is_empty()
        || release.runtime_version.len() > 64
        || release.source_revision.is_empty()
        || release.source_revision.len() > 128
        || release.pack_fingerprint.is_empty()
        || release.pack_fingerprint.len() > 1024
        || release.license_policy.is_empty()
        || release.license_policy.len() > 128
        || release.license_files.is_empty()
        || release.license_files.len() > 64
        || release.unpacked_length == 0
        || release.unpacked_length > MODEL_UNPACKED_LIMIT
        || release.rollout_percent > 100
        || release.rollout_seed.is_empty()
        || release.rollout_seed.len() > 128
        || length == 0
        || length > MODEL_PACKAGE_LIMIT
    {
        return Err(Error::Policy(
            "invalid model release bounds or identity fields",
        ));
    }
    for name in &release.license_files {
        validate_model_id(name)?;
    }
    if path
        != format!(
            "models/{}/{}/{}/{}/model-pack.zip",
            release.model_id, release.model_version, release.platform, release.release_id
        )
    {
        return Err(Error::Policy("model path disagrees with signed identity"));
    }
    Ok(())
}
pub(crate) fn validate_model_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || matches!(id, "." | "..")
        || id.ends_with('.')
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::Policy(
            "unsafe model identifier or inventory basename",
        ));
    }
    Ok(())
}
fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Policy(
            "model content hashes must be lowercase SHA-256",
        ));
    }
    Ok(())
}
