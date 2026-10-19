//! Public-only compile-time bootstrap. Source review is the trust handoff;
//! self-signatures establish internal validity, not a publisher's identity.
use crate::{Error, METADATA_LIMIT, PRODUCT, ProductionConfig, PublisherPolicy, Result};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

// This candidate is deliberately unprovisioned. A separately authorized source
// build may replace None with ONLY these two reviewed public inputs:
// Some((include_bytes!("../bootstrap/public-bundle.json"),
//       include_bytes!("../bootstrap/root.json")));
// No environment, feature flag, runtime file or remote setting selects this.
const COMPILED_PUBLIC_BUNDLE: Option<(&[u8], &[u8])> = None;
const COMPILED_ORIGIN: &str = "https://175.24.190.175";
const COMPILED_PLATFORM: &str = "windows-x64";
const COMPILED_CHANNEL: &str = "stable";
const MANIFEST_LIMIT: usize = 4096;

pub(crate) fn bundled() -> Result<ProductionConfig> {
    match COMPILED_PUBLIC_BUNDLE {
        Some((manifest, root)) => from_compiled_public_anchor(manifest, root),
        None => Err(Error::NotConfigured(
            "a reviewed embedded TUF root and verified IP HTTPS service are not provisioned",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicBundle {
    schema: u32,
    product: String,
    platform: String,
    channel: String,
    repository_origin: String,
    publisher_policy: String,
    key_profile: KeyProfile,
    root_version: u64,
    root_length: u64,
    root_sha256: String,
}
#[derive(Deserialize)]
enum KeyProfile {
    #[serde(rename = "root-2-of-3-six-keys")]
    Six,
    #[serde(rename = "four-role-1-of-1")]
    Four,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    signed: PublicRoot,
    signatures: Vec<PublicSignature>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicSignature {
    keyid: String,
    sig: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicRoot {
    #[serde(rename = "_type")]
    kind: String,
    spec_version: String,
    version: u64,
    consistent_snapshot: bool,
    issued: String,
    expires: String,
    #[serde(deserialize_with = "unique_map")]
    keys: BTreeMap<String, PublicKey>,
    #[serde(deserialize_with = "unique_map")]
    roles: BTreeMap<String, PublicRole>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicKey {
    keytype: String,
    scheme: String,
    keyval: PublicKeyValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicKeyValue {
    public: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicRole {
    keyids: Vec<String>,
    threshold: u32,
}
fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct UniqueMap<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for UniqueMap<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map with unique public key/role names")
        }
        fn visit_map<M: MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate public key/role name"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(UniqueMap(std::marker::PhantomData))
}
fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn from_reviewed_public_bundle(
    manifest: &[u8],
    root_bytes: &[u8],
) -> Result<ProductionConfig> {
    validate_bundle(manifest, root_bytes, true)
}

/// A historical immutable anchor may expire after authentic root rotation.
/// Only the compiled-byte entry point uses this path. The unchanged TUF refresh
/// must still establish a fresh final root and metadata before selecting targets.
pub(crate) fn from_compiled_public_anchor(
    manifest: &[u8],
    root_bytes: &[u8],
) -> Result<ProductionConfig> {
    validate_bundle(manifest, root_bytes, false)
}

fn validate_bundle(
    manifest: &[u8],
    root_bytes: &[u8],
    require_fresh_provisioning: bool,
) -> Result<ProductionConfig> {
    if manifest.is_empty()
        || manifest.len() > MANIFEST_LIMIT
        || root_bytes.is_empty()
        || root_bytes.len() as u64 > METADATA_LIMIT
    {
        return Err(Error::Policy(
            "missing or oversized public bootstrap bundle",
        ));
    }
    let bundle: PublicBundle = serde_json::from_slice(manifest)?;
    if bundle.schema != 1
        || bundle.product != PRODUCT
        || bundle.platform != COMPILED_PLATFORM
        || bundle.channel != COMPILED_CHANNEL
        || bundle.repository_origin != COMPILED_ORIGIN
        || bundle.publisher_policy != "tuf-authenticated-unsigned"
        || bundle.root_length != root_bytes.len() as u64
        || !lower_hex(&bundle.root_sha256, 64)
        || bundle.root_sha256 != hex::encode(Sha256::digest(root_bytes))
    {
        return Err(Error::Policy(
            "public bootstrap differs from compiled pins or root bytes",
        ));
    }
    let envelope: Envelope = serde_json::from_slice(root_bytes)?;
    let root = &envelope.signed;
    if root.kind != "root"
        || root.spec_version != "1.0.0"
        || !root.consistent_snapshot
        || root.version != bundle.root_version
    {
        return Err(Error::Policy("unsupported public root profile or version"));
    }
    let issued: jiff::Timestamp = root
        .issued
        .parse()
        .map_err(|_| Error::Policy("invalid root issuance"))?;
    let expires: jiff::Timestamp = root
        .expires
        .parse()
        .map_err(|_| Error::Policy("invalid root expiry"))?;
    let now = jiff::Timestamp::now().as_second();
    if issued.as_second() > now
        || (require_fresh_provisioning && expires.as_second() <= now)
        || expires.as_second() <= issued.as_second()
        || expires
            .as_second()
            .checked_sub(issued.as_second())
            .is_none_or(|duration| duration > 366 * 86400)
    {
        return Err(Error::Policy(
            "future, expired or excessive public root lifetime",
        ));
    }
    let (key_count, root_count, root_threshold) = match bundle.key_profile {
        KeyProfile::Six => (6, 3, 2),
        KeyProfile::Four => (4, 1, 1),
    };
    if root.keys.len() != key_count || root.roles.len() != 4 {
        return Err(Error::Policy(
            "public root key/role count differs from profile",
        ));
    }
    for (id, key) in &root.keys {
        if key.keytype != "ed25519" || key.scheme != "ed25519" || !lower_hex(&key.keyval.public, 64)
        {
            return Err(Error::Policy(
                "public root requires Ed25519 public keys only",
            ));
        }
        // Only fixed ASCII fields/values occur here, so sorted JSON equals the
        // TUF canonical JSON representation for this exact public-key schema.
        let canonical_key = format!(
            r#"{{"keytype":"ed25519","keyval":{{"public":"{}"}},"scheme":"ed25519"}}"#,
            key.keyval.public
        );
        if id != &hex::encode(Sha256::digest(canonical_key)) {
            return Err(Error::Policy(
                "public key ID differs from canonical key digest",
            ));
        }
    }
    let mut assigned = BTreeSet::new();
    for name in ["root", "targets", "snapshot", "timestamp"] {
        let role = root
            .roles
            .get(name)
            .ok_or(Error::Policy("required public root role missing"))?;
        let (count, threshold) = if name == "root" {
            (root_count, root_threshold)
        } else {
            (1, 1)
        };
        if role.keyids.len() != count || role.threshold != threshold {
            return Err(Error::Policy(
                "public root role threshold differs from profile",
            ));
        }
        for id in &role.keyids {
            if !root.keys.contains_key(id) || !assigned.insert(id) {
                return Err(Error::Policy(
                    "public role keys missing, repeated or shared",
                ));
            }
        }
    }
    let root_ids = &root.roles["root"].keyids;
    let mut signers = BTreeSet::new();
    for signature in &envelope.signatures {
        if !root_ids.contains(&signature.keyid)
            || !signers.insert(&signature.keyid)
            || !lower_hex(&signature.sig, 128)
        {
            return Err(Error::Policy("invalid or repeated public root signature"));
        }
    }
    // Policy, origin and scope come from compiled constants, never from the
    // manifest values. Their exact equality above is a review-error guard.
    ProductionConfig::new(
        root_bytes,
        COMPILED_ORIGIN,
        PublisherPolicy::TufAuthenticatedUnsigned,
        COMPILED_PLATFORM,
        COMPILED_CHANNEL,
    )
}
