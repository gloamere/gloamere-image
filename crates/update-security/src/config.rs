use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tough::schema::{Root, Signed};
use url::Url;

pub const PRODUCT: &str = "gloamere-image";
pub const METADATA_LIMIT: u64 = 1024 * 1024;
pub const APP_PACKAGE_LIMIT: u64 = 64 * 1024 * 1024;
pub const MODEL_PACKAGE_LIMIT: u64 = 256 * 1024 * 1024;
pub const MODEL_UNPACKED_LIMIT: u64 = 512 * 1024 * 1024;
pub const MAX_METADATA_VERSION: u64 = 1_000_000_000;
pub const MAX_ROOT_CHAIN: usize = 128;

/// Immutable build policy, never accepted from repository custom fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublisherPolicy {
    /// Windows platform layer must verify system trust and this exact leaf DER SHA-256.
    ExactAuthenticodeCertificate([u8; 32]),
    /// Explicit user-approved v0.7 policy while no code-signing certificate exists.
    /// TUF authenticity and HTTPS are still mandatory. A present invalid signature
    /// must be rejected rather than treated as an unsigned package.
    TufAuthenticatedUnsigned,
}

#[derive(Clone, Debug)]
pub struct ProductionConfig {
    pub(crate) pinned_root: Vec<u8>,
    pub(crate) root_identity: [u8; 32],
    pub(crate) metadata_url: Url,
    pub(crate) targets_url: Url,
    pub(crate) origin: String,
    pub(crate) platform: String,
    pub(crate) channel: String,
    pub(crate) publisher_policy: PublisherPolicy,
}

impl ProductionConfig {
    /// This candidate has no public bootstrap bundle. A separately reviewed
    /// source build may embed one; no runtime/environment switch can enable it.
    pub fn bundled() -> Result<Self> {
        crate::bootstrap::bundled()
    }

    /// Validate only a separately reviewed, public-only local/build artifact.
    /// Never call this with a downloaded/adjacent user-writable configuration.
    /// Self-signatures do not prove publisher identity: verify the public-root
    /// fingerprint through an independent authorized handoff before embedding.
    /// This does not alter the current build, install keys, reset state or
    /// configure HTTPS. `bundled()` uses immutable compiled bytes only.
    pub fn from_reviewed_public_bundle(manifest: &[u8], root: &[u8]) -> Result<Self> {
        crate::bootstrap::from_reviewed_public_bundle(manifest, root)
    }

    /// Call only from reviewed local/build configuration, never unsigned JSON,
    /// environment variables, server metadata or user-selected update URLs.
    pub fn new(
        pinned_root: &[u8],
        repository_origin: &str,
        publisher_policy: PublisherPolicy,
        platform: &str,
        channel: &str,
    ) -> Result<Self> {
        if pinned_root.is_empty() || pinned_root.len() as u64 > METADATA_LIMIT {
            return Err(Error::Policy("missing or oversized embedded root"));
        }
        let root: Signed<Root> = serde_json::from_slice(pinned_root)?;
        root.signed
            .verify_role(&root)
            .map_err(|e| Error::Tuf(e.to_string()))?;
        if !root.signed.consistent_snapshot || root.signed.version.get() > MAX_METADATA_VERSION {
            return Err(Error::Policy(
                "consistent snapshots and bounded root version required",
            ));
        }
        if !matches!(
            platform,
            "windows-x64" | "macos-arm64" | "macos-x64" | "linux-x64"
        ) || !matches!(channel, "stable" | "experimental")
        {
            return Err(Error::Policy("unsupported compiled platform/channel"));
        }
        let origin = Url::parse(repository_origin)
            .map_err(|_| Error::Policy("invalid repository origin"))?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || repository_origin.contains('\\')
            || repository_origin.chars().any(|c| c.is_ascii_control())
            || repository_origin.contains('%')
            || repository_origin
                .split('/')
                .any(|s| matches!(s, "." | ".."))
        {
            return Err(Error::Policy("repository must be an exact HTTPS origin"));
        }
        if let PublisherPolicy::ExactAuthenticodeCertificate(hash) = publisher_policy
            && hash == [0; 32]
        {
            return Err(Error::Policy("publisher pin must not be empty"));
        }
        let canonical_origin = origin.origin().ascii_serialization();
        Ok(Self {
            pinned_root: pinned_root.to_vec(),
            root_identity: Sha256::digest(pinned_root).into(),
            metadata_url: origin
                .join("metadata/")
                .map_err(|_| Error::Policy("invalid metadata URL"))?,
            targets_url: origin,
            origin: canonical_origin,
            platform: platform.to_owned(),
            channel: channel.to_owned(),
            publisher_policy,
        })
    }

    pub fn publisher_policy(&self) -> &PublisherPolicy {
        &self.publisher_policy
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn platform(&self) -> &str {
        &self.platform
    }
    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub(crate) fn binding(&self) -> String {
        // Changing trust, scope, origin or executable policy needs explicit state migration.
        let data = serde_json::to_vec(&(
            self.root_identity,
            &self.origin,
            &self.platform,
            &self.channel,
            &self.publisher_policy,
        ))
        .expect("serialize simple compiled configuration");
        hex::encode(Sha256::digest(data))
    }
}
