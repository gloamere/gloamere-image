//! Ephemeral signing keys exist only in this test process. No signing secret is
//! saved in fixtures, source distributions or production configuration.
use super::*;
use crate::metadata::refresh_with_transport;
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tough::{Bytes, Transport, TransportError, TransportErrorKind, TransportStream};
use url::Url;

#[derive(Clone, Debug, Default)]
struct FixtureTransport(Arc<Mutex<BTreeMap<String, Vec<u8>>>>);
#[tough::async_trait]
impl Transport for FixtureTransport {
    async fn fetch(&self, url: Url) -> std::result::Result<TransportStream, TransportError> {
        let bytes = self
            .0
            .lock()
            .unwrap()
            .get(url.path())
            .cloned()
            .ok_or_else(|| TransportError::new(TransportErrorKind::FileNotFound, &url))?;
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from(
            bytes,
        ))])))
    }
}
impl FixtureTransport {
    fn put(&self, name: &str, bytes: Vec<u8>) {
        self.0
            .lock()
            .unwrap()
            .insert(format!("/metadata/{name}"), bytes);
    }
}

struct TestKey {
    signing: SigningKey,
    public: Value,
    id: String,
}
impl TestKey {
    fn new() -> Self {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let public = json!({"keytype":"ed25519","scheme":"ed25519","keyval":{"public":hex::encode(signing.verifying_key().to_bytes())}});
        let id = hex::encode(Sha256::digest(canonical(&public)));
        Self {
            signing,
            public,
            id,
        }
    }
    fn signature(&self, body: &Value) -> Value {
        json!({"keyid":self.id,"sig":hex::encode(self.signing.sign(&canonical(body)).to_bytes())})
    }
}
fn canonical(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut bytes, olpc_cjson::CanonicalFormatter::new());
    value.serialize(&mut serializer).unwrap();
    bytes
}
#[test]
fn signed_target_identity_survives_map_order_and_reparse_but_binds_all_content() {
    let hash = "a".repeat(64);
    let first = format!(
        r#"{{"length":1,"hashes":{{"sha256":"{hash}"}},"custom":{{"a":1,"b":{{"x":2,"y":3}}}}}}"#
    );
    let reversed = format!(
        r#"{{"custom":{{"b":{{"y":3,"x":2}},"a":1}},"hashes":{{"sha256":"{hash}"}},"length":1}}"#
    );
    let original: tough::schema::Target = serde_json::from_str(&first).unwrap();
    let name = "releases/windows-x64/stable/71/Setup.exe";
    let identity = crate::state::signed_target_identity(name, &original).unwrap();
    for _ in 0..32 {
        let reparsed: tough::schema::Target = serde_json::from_str(&reversed).unwrap();
        assert_eq!(
            crate::state::signed_target_identity(name, &reparsed).unwrap(),
            identity
        );
    }
    let changed: tough::schema::Target =
        serde_json::from_str(&first.replace("\"x\":2", "\"x\":4")).unwrap();
    assert_ne!(
        crate::state::signed_target_identity(name, &changed).unwrap(),
        identity
    );
    assert_ne!(
        crate::state::signed_target_identity("different/Setup.exe", &original).unwrap(),
        identity
    );
}
fn signed(body: &Value, keys: &[&TestKey]) -> Vec<u8> {
    serde_json::to_vec(&json!({"signed":body,"signatures":keys.iter().map(|key|key.signature(body)).collect::<Vec<_>>()})).unwrap()
}
fn ts(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds).unwrap().to_string()
}
fn reference(bytes: &[u8], version: u64) -> Value {
    json!({"version":version,"length":bytes.len(),"hashes":{"sha256":hex::encode(Sha256::digest(bytes))}})
}
struct Fixture {
    root_key: TestKey,
    timestamp_key: TestKey,
    snapshot_key: TestKey,
    targets_key: TestKey,
    root: Value,
    timestamp: Value,
    snapshot: Value,
    targets: Value,
    transport: FixtureTransport,
    now: i64,
}
impl Fixture {
    fn new() -> Self {
        let root_key = TestKey::new();
        let timestamp_key = TestKey::new();
        let snapshot_key = TestKey::new();
        let targets_key = TestKey::new();
        let now = jiff::Timestamp::now().as_second() - 2;
        let mut keys = serde_json::Map::new();
        for key in [&root_key, &timestamp_key, &snapshot_key, &targets_key] {
            keys.insert(key.id.clone(), key.public.clone());
        }
        let role = |key: &TestKey| json!({"keyids":[key.id],"threshold":1});
        let root = json!({"_type":"root","spec_version":"1.0.0","version":1,"consistent_snapshot":true,
            "issued":ts(now),"expires":ts(now+30*86400),"keys":keys,
            "roles":{"root":role(&root_key),"timestamp":role(&timestamp_key),"snapshot":role(&snapshot_key),"targets":role(&targets_key)}});
        let mut result = Self {
            root_key,
            timestamp_key,
            snapshot_key,
            targets_key,
            root,
            timestamp: Value::Null,
            snapshot: Value::Null,
            targets: Value::Null,
            transport: FixtureTransport::default(),
            now,
        };
        result.publish(1, 71);
        result
    }
    fn root_bytes(&self) -> Vec<u8> {
        signed(&self.root, &[&self.root_key])
    }
    fn config(&self) -> ProductionConfig {
        ProductionConfig::new(
            &self.root_bytes(),
            "https://175.24.190.175",
            PublisherPolicy::TufAuthenticatedUnsigned,
            "windows-x64",
            "stable",
        )
        .unwrap()
    }
    fn publish(&mut self, version: u64, release: u64) {
        let package = b"MZephemeral test bytes";
        let name = format!("releases/windows-x64/stable/{release}/Setup.exe");
        self.targets = json!({"_type":"targets","spec_version":"1.0.0","version":version,
            "issued":ts(self.now),"expires":ts(self.now+7*86400),"targets":{name:{"length":package.len(),"hashes":{"sha256":hex::encode(Sha256::digest(package))},
            "custom":{"product":PRODUCT,"kind":"app","version":"0.7.1","release_id":release,"platform":"windows-x64","channel":"stable","install_scope":"current-user",
                "min_updater_protocol":1,"min_launcher_protocol":1,"settings_compat":[1],"ai_worker_protocol":2,"rollout_percent":100,"rollout_seed":"test","notes_path":"notes/71.zh-CN.txt"}}}});
        self.snapshot = json!({"_type":"snapshot","spec_version":"1.0.0","version":version,
            "issued":ts(self.now),"expires":ts(self.now+7*86400),"meta":{}});
        self.timestamp = json!({"_type":"timestamp","spec_version":"1.0.0","version":version,
            "issued":ts(self.now),"expires":ts(self.now+86400),"meta":{}});
        self.rebind();
    }
    fn rebind(&mut self) {
        let targets = signed(&self.targets, &[&self.targets_key]);
        let tv = self.targets["version"].as_u64().unwrap();
        self.snapshot["meta"]["targets.json"] = reference(&targets, tv);
        self.transport.put(&format!("{tv}.targets.json"), targets);
        self.rebind_snapshot();
    }
    fn rebind_snapshot(&mut self) {
        let snapshot = signed(&self.snapshot, &[&self.snapshot_key]);
        let sv = self.snapshot["version"].as_u64().unwrap();
        self.timestamp["meta"]["snapshot.json"] = reference(&snapshot, sv);
        self.transport.put(&format!("{sv}.snapshot.json"), snapshot);
        self.resign_timestamp();
    }
    fn resign_timestamp(&self) {
        self.transport.put(
            "timestamp.json",
            signed(&self.timestamp, &[&self.timestamp_key]),
        );
    }
    fn target_mut(&mut self) -> &mut Value {
        self.targets["targets"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
    }
}
fn compatibility() -> Compatibility {
    Compatibility {
        installed_release_id: 70,
        updater_protocol: 1,
        launcher_protocol: 1,
        settings_schema: 1,
        ai_worker_protocol: 2,
        installation_id: "ephemeral-installation".into(),
    }
}
fn initialized(config: &ProductionConfig) -> (tempfile::TempDir, LockedState) {
    let parent = tempfile::tempdir().unwrap();
    let state = LockedState::initialize(&parent.path().join("trust"), config).unwrap();
    (parent, state)
}
async fn load(
    fixture: &Fixture,
    config: &ProductionConfig,
    state: &mut LockedState,
) -> Result<TrustedRepository> {
    refresh_with_transport(
        config,
        state,
        &CancellationToken::new(),
        fixture.transport.clone(),
    )
    .await
}

#[test]
fn production_without_trust_is_disabled() {
    assert!(matches!(
        ProductionConfig::bundled(),
        Err(Error::NotConfigured(_))
    ));
}

#[test]
fn direct_ip_requires_https_and_exact_origin() {
    let f = Fixture::new();
    for origin in [
        "http://175.24.190.175",
        "https://user@175.24.190.175",
        "https://175.24.190.175/metadata/",
        "https://175.24.190.175/?x=y",
    ] {
        assert!(
            ProductionConfig::new(
                &f.root_bytes(),
                origin,
                PublisherPolicy::TufAuthenticatedUnsigned,
                "windows-x64",
                "stable"
            )
            .is_err(),
            "{origin}"
        );
    }
    assert_eq!(f.config().origin(), "https://175.24.190.175");
}

#[tokio::test]
async fn authentic_tuf_release_is_selected_with_consistent_snapshot_url() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let repo = load(&f, &c, &mut s).await.unwrap();
    let target = repo
        .select_app(&c, &mut s, &compatibility())
        .unwrap()
        .unwrap();
    assert_eq!(target.release().release_id, 71);
    assert!(
        target
            .url()
            .ends_with(&format!("{}.Setup.exe", hex::encode(target.sha256())))
    );
    assert_eq!(s.snapshot().highest_metadata["timestamp"].version, 1);
}

#[tokio::test]
async fn wrong_role_key_cannot_authorize_metadata() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    f.transport
        .put("timestamp.json", signed(&f.timestamp, &[&f.targets_key]));
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[tokio::test]
async fn corrupted_signature_is_rejected() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut value: Value =
        serde_json::from_slice(&signed(&f.timestamp, &[&f.timestamp_key])).unwrap();
    value["signed"]["expires"] = json!(ts(f.now + 86401));
    f.transport
        .put("timestamp.json", serde_json::to_vec(&value).unwrap());
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[tokio::test]
async fn signed_body_unknown_fields_are_covered_by_signature() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut value: Value =
        serde_json::from_slice(&signed(&f.timestamp, &[&f.timestamp_key])).unwrap();
    value["signed"]["unknown_security_field"] = json!("changed");
    f.transport
        .put("timestamp.json", serde_json::to_vec(&value).unwrap());
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[test]
fn repeated_signature_cannot_meet_two_key_threshold() {
    let mut f = Fixture::new();
    f.root["roles"]["root"] = json!({"keyids":[f.root_key.id,f.targets_key.id],"threshold":2});
    let root = signed(&f.root, &[&f.root_key, &f.root_key]);
    assert!(
        ProductionConfig::new(
            &root,
            "https://175.24.190.175",
            PublisherPolicy::TufAuthenticatedUnsigned,
            "windows-x64",
            "stable"
        )
        .is_err()
    );
}

#[tokio::test]
async fn replay_is_rejected_across_reopen() {
    let mut f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    f.publish(2, 72);
    load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap();
    drop(s);
    let mut s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    f.publish(1, 71);
    assert!(load(&f, &c, &mut s).await.is_err());
    assert_eq!(s.snapshot().highest_metadata["timestamp"].version, 2);
}

#[tokio::test]
async fn same_version_equivocation_is_rejected() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    load(&f, &c, &mut s).await.unwrap();
    f.timestamp["expires"] = json!(ts(f.now + 86401));
    f.resign_timestamp();
    assert!(matches!(
        load(&f, &c, &mut s).await,
        Err(Error::Rollback(_))
    ));
}

#[tokio::test]
async fn newer_metadata_cannot_replay_older_release() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap();
    f.publish(2, 70);
    assert!(matches!(
        load(&f, &c, &mut s)
            .await
            .unwrap()
            .select_app(&c, &mut s, &compatibility()),
        Err(Error::Rollback(_))
    ));
}

#[tokio::test]
async fn same_release_sequence_cannot_change_package_or_policy() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap();
    f.publish(2, 71);
    f.target_mut()["custom"]["version"] = json!("0.7.2");
    f.rebind();
    assert!(matches!(
        load(&f, &c, &mut s)
            .await
            .unwrap()
            .select_app(&c, &mut s, &compatibility()),
        Err(Error::Rollback(_))
    ));
}

#[tokio::test]
async fn newer_timestamp_floor_survives_later_snapshot_failure() {
    let mut f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    load(&f, &c, &mut s).await.unwrap();
    f.publish(2, 72);
    f.transport.put("2.snapshot.json", b"corrupt".to_vec());
    assert!(load(&f, &c, &mut s).await.is_err());
    assert_eq!(s.snapshot().highest_metadata["timestamp"].version, 2);
    drop(s);
    let mut s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    f.publish(1, 71);
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[tokio::test]
async fn metadata_mix_and_match_is_rejected() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut replaced = f.targets.clone();
    replaced["expires"] = json!(ts(f.now + 6 * 86400));
    f.transport
        .put("1.targets.json", signed(&replaced, &[&f.targets_key]));
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[tokio::test]
async fn future_and_expired_timestamp_rejected() {
    for future in [true, false] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        if future {
            f.timestamp["issued"] = json!(ts(f.now + 120));
        } else {
            f.timestamp["expires"] = json!(ts(f.now - 1));
        }
        f.resign_timestamp();
        assert!(load(&f, &c, &mut s).await.is_err());
    }
}

#[tokio::test]
async fn missing_signed_issue_time_and_excessive_validity_rejected() {
    for missing in [true, false] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        if missing {
            f.timestamp.as_object_mut().unwrap().remove("issued");
        } else {
            f.timestamp["expires"] = json!(ts(f.now + 100 * 86400));
        }
        f.resign_timestamp();
        assert!(load(&f, &c, &mut s).await.is_err());
    }
}

#[tokio::test]
async fn missing_metadata_length_rejected_even_though_tuf_allows_optional_length() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    f.timestamp["meta"]["snapshot.json"]
        .as_object_mut()
        .unwrap()
        .remove("length");
    f.resign_timestamp();
    assert!(load(&f, &c, &mut s).await.is_err());
}

#[tokio::test]
async fn missing_zero_huge_and_overflow_target_lengths_rejected() {
    for length in [
        Value::Null,
        json!(0),
        json!(u64::MAX),
        json!("18446744073709551616"),
    ] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        if length.is_null() {
            f.target_mut().as_object_mut().unwrap().remove("length");
        } else {
            f.target_mut()["length"] = length;
        }
        f.rebind();
        assert!(load(&f, &c, &mut s).await.is_err());
    }
}

#[tokio::test]
async fn traversal_target_name_is_rejected_without_normalization() {
    for name in [
        "../Setup.exe",
        "/Setup.exe",
        "releases/../Setup.exe",
        "releases/%2e%2e/Setup.exe",
        "C:\\Setup.exe",
        "a//Setup.exe",
    ] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        let target = f.target_mut().clone();
        f.targets["targets"] = json!({name:target});
        f.rebind();
        assert!(load(&f, &c, &mut s).await.is_err(), "{name}");
    }
}

#[tokio::test]
async fn remote_approved_and_commands_are_not_release_authority() {
    for field in [
        "approved",
        "command",
        "arguments",
        "install_dir",
        "publisher_policy",
    ] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        f.target_mut()["custom"][field] = json!(true);
        f.rebind();
        let repo = load(&f, &c, &mut s).await.unwrap();
        assert!(
            repo.select_app(&c, &mut s, &compatibility()).is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn model_target_does_not_authorize_app_or_stable_quality() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    f.target_mut()["custom"]["kind"] = json!("model");
    f.target_mut()["custom"]["approved"] = json!(true);
    f.rebind();
    assert!(
        load(&f, &c, &mut s)
            .await
            .unwrap()
            .select_app(&c, &mut s, &compatibility())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn compatibility_is_separate_from_authenticity() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut comp = compatibility();
    comp.ai_worker_protocol = 99;
    assert!(
        load(&f, &c, &mut s)
            .await
            .unwrap()
            .select_app(&c, &mut s, &comp)
            .is_err()
    );
    assert_eq!(s.state.releases.values().next().unwrap().sequence, 71);
}

#[tokio::test]
async fn health_rollback_keeps_floors_and_quarantines_release() {
    let f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap();
    s.quarantine_app_release(71).unwrap();
    drop(s);
    let mut s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    assert!(
        load(&f, &c, &mut s)
            .await
            .unwrap()
            .select_app(&c, &mut s, &compatibility())
            .unwrap()
            .is_none()
    );
    assert_eq!(s.state.releases.values().next().unwrap().sequence, 71);
}

#[tokio::test]
async fn clock_rollback_blocks_before_network() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut next = s.state.clone();
    next.last_observed_unix_seconds = jiff::Timestamp::now().as_second() + 600;
    s.commit(next).unwrap();
    assert!(matches!(
        load(&f, &c, &mut s).await,
        Err(Error::Rollback(_))
    ));
}

#[test]
fn state_corruption_missing_state_and_concurrent_open_fail_closed() {
    let f = Fixture::new();
    let c = f.config();
    let (p, s) = initialized(&c);
    let path = p.path().join("trust");
    assert!(matches!(LockedState::open(&path, &c), Err(Error::Locked)));
    drop(s);
    std::fs::write(path.join("trusted-state-v1.json"), b"{}").unwrap();
    assert!(LockedState::open(&path, &c).is_err());
    assert!(LockedState::initialize(&path, &c).is_err());
    std::fs::remove_file(path.join("trusted-state-v1.json")).unwrap();
    assert!(LockedState::open(&path, &c).is_err());
}

#[test]
fn lower_internal_floor_cannot_commit() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let mut next = s.state.clone();
    next.highest_metadata.get_mut("root").unwrap().version = 0;
    assert!(matches!(s.commit(next), Err(Error::Rollback(_))));
}

#[tokio::test]
async fn cancelled_refresh_makes_no_trust_progress() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        refresh_with_transport(&c, &mut s, &cancel, f.transport.clone()).await,
        Err(Error::Cancelled)
    ));
    assert_eq!(s.snapshot().highest_metadata.len(), 1);
}

#[tokio::test]
async fn sequential_root_rotation_requires_old_and_new_keys_and_survives_reopen() {
    let f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    let new_key = TestKey::new();
    let mut root = f.root.clone();
    root["version"] = json!(2);
    root["keys"][&new_key.id] = new_key.public.clone();
    root["roles"]["root"] = json!({"keyids":[new_key.id],"threshold":1});
    f.transport
        .put("2.root.json", signed(&root, &[&f.root_key, &new_key]));
    load(&f, &c, &mut s).await.unwrap();
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
    drop(s);
    let mut s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    load(&f, &c, &mut s).await.unwrap();
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
}

#[tokio::test]
async fn skipped_or_single_authority_root_rotation_rejected() {
    for mode in [0, 1, 2] {
        let f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        let new_key = TestKey::new();
        let mut root = f.root.clone();
        root["version"] = json!(if mode == 0 { 3 } else { 2 });
        root["keys"][&new_key.id] = new_key.public.clone();
        root["roles"]["root"] = json!({"keyids":[new_key.id],"threshold":1});
        let keys = match mode {
            1 => vec![&f.root_key],
            2 => vec![&new_key],
            _ => vec![&f.root_key, &new_key],
        };
        f.transport.put("2.root.json", signed(&root, &keys));
        assert!(load(&f, &c, &mut s).await.is_err());
        assert_eq!(s.snapshot().highest_metadata["root"].version, 1);
    }
}

#[tokio::test]
async fn root_progress_persists_when_timestamp_is_missing() {
    let f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    let mut root = f.root.clone();
    root["version"] = json!(2);
    f.transport
        .put("2.root.json", signed(&root, &[&f.root_key]));
    f.transport
        .0
        .lock()
        .unwrap()
        .remove("/metadata/timestamp.json");
    assert!(load(&f, &c, &mut s).await.is_err());
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
    drop(s);
    let s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
}

#[tokio::test]
async fn execution_revalidation_rejects_quarantine_and_newer_metadata() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let target = load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap()
        .unwrap();
    revalidate_for_execution(&c, &target, &mut s).unwrap();
    f.publish(2, 72);
    load(&f, &c, &mut s).await.unwrap();
    assert!(matches!(
        revalidate_for_execution(&c, &target, &mut s),
        Err(Error::Rollback(_))
    ));
    let target = load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap()
        .unwrap();
    s.quarantine_app_release(72).unwrap();
    assert!(revalidate_for_execution(&c, &target, &mut s).is_err());
}

#[tokio::test]
async fn root_rotation_survives_later_timestamp_rollback_rejection() {
    let mut f = Fixture::new();
    let c = f.config();
    let (p, mut s) = initialized(&c);
    f.publish(2, 72);
    load(&f, &c, &mut s).await.unwrap();
    let mut root = f.root.clone();
    root["version"] = json!(2);
    f.transport
        .put("2.root.json", signed(&root, &[&f.root_key]));
    f.publish(1, 71);
    assert!(load(&f, &c, &mut s).await.is_err());
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
    drop(s);
    let s = LockedState::open(&p.path().join("trust"), &c).unwrap();
    assert_eq!(s.snapshot().highest_metadata["root"].version, 2);
    assert_eq!(s.snapshot().highest_metadata["timestamp"].version, 2);
}

#[cfg(unix)]
#[test]
fn state_rejects_symlink_ancestors_and_dangling_lock() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let c = f.config();
    let (p, s) = initialized(&c);
    drop(s);
    symlink(p.path().join("trust"), p.path().join("linked")).unwrap();
    assert!(LockedState::open(&p.path().join("linked"), &c).is_err());
    std::fs::remove_file(p.path().join("trust/trust.lock")).unwrap();
    let external = p.path().join("external");
    symlink(&external, p.path().join("trust/trust.lock")).unwrap();
    assert!(LockedState::open(&p.path().join("trust"), &c).is_err());
    assert!(!external.exists());
}

#[tokio::test]
async fn expired_root_and_snapshot_are_rejected() {
    for role in ["root", "snapshot", "targets"] {
        let mut f = Fixture::new();
        match role {
            "root" => f.root["expires"] = json!(ts(f.now - 1)),
            "snapshot" => {
                f.snapshot["expires"] = json!(ts(f.now - 1));
                f.rebind_snapshot();
            }
            "targets" => {
                f.targets["expires"] = json!(ts(f.now - 1));
                f.rebind();
            }
            _ => unreachable!(),
        }
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        assert!(load(&f, &c, &mut s).await.is_err(), "{role}");
    }
}

#[tokio::test]
async fn signed_wrong_product_platform_channel_and_kind_are_not_selected() {
    for (field, value) in [
        ("product", "other-product"),
        ("platform", "macos-arm64"),
        ("channel", "experimental"),
        ("kind", "model"),
    ] {
        let mut f = Fixture::new();
        let c = f.config();
        let (_p, mut s) = initialized(&c);
        f.target_mut()["custom"][field] = json!(value);
        if field == "platform" || field == "channel" {
            let target = f.target_mut().clone();
            let platform = target["custom"]["platform"].as_str().unwrap();
            let channel = target["custom"]["channel"].as_str().unwrap();
            let name = format!("releases/{platform}/{channel}/71/Setup.exe");
            f.targets["targets"] = json!({name:target});
        }
        f.rebind();
        assert!(
            load(&f, &c, &mut s)
                .await
                .unwrap()
                .select_app(&c, &mut s, &compatibility())
                .unwrap()
                .is_none(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn superseded_repository_cannot_reauthorize_removed_old_release() {
    let mut f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let old = load(&f, &c, &mut s).await.unwrap();
    f.publish(2, 72);
    load(&f, &c, &mut s).await.unwrap();
    assert!(matches!(
        old.select_app(&c, &mut s, &compatibility()),
        Err(Error::Rollback(_))
    ));
}

#[tokio::test]
async fn selected_target_clock_and_expiry_are_rechecked_before_download() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut s) = initialized(&c);
    let target = load(&f, &c, &mut s)
        .await
        .unwrap()
        .select_app(&c, &mut s, &compatibility())
        .unwrap()
        .unwrap();
    assert!(
        target
            .ensure_fresh_at(jiff::Timestamp::now().as_second())
            .is_ok()
    );
    assert!(
        target
            .ensure_fresh_at(jiff::Timestamp::now().as_second() - 600)
            .is_err()
    );
    assert!(
        target
            .ensure_fresh_at(target.valid_until_unix_seconds())
            .is_err()
    );
}

fn model_compatibility() -> ModelCompatibility {
    ModelCompatibility {
        installed_release_id: 0,
        app_protocol: 1,
        installation_id: "model-fixture-installation".into(),
        model_id: "code-linear-fp32-experimental".into(),
        model_version: "a".repeat(64),
        pack_schema: 2,
        preprocess_version: "fixed-preprocess-v2".into(),
        worker_protocol: 1,
        runtime_version: "1.28.3".into(),
        source_revision: "reviewed-source-revision".into(),
        pack_fingerprint: "compiled-weights-runtime-decoder-threshold-fingerprint".into(),
        license_policy: "gloamere-code-ort1283-ljt3141-license-set-v1".into(),
        license_files: vec![
            "CoDE-MIT.txt".into(),
            "ONNXRuntime-LICENSE".into(),
            "JPEG-PROVENANCE.json".into(),
        ],
        manifest_sha256: "b".repeat(64),
        unpacked_length: 1024,
    }
}
impl Fixture {
    fn add_model(&mut self, sequence: u64) {
        let c = model_compatibility();
        let archive = b"PKephemeral model ZIP bytes are never executed";
        let name = format!(
            "models/{}/{}/windows-x64/{sequence}/model-pack.zip",
            c.model_id, c.model_version
        );
        self.targets["targets"][name] = json!({"length":archive.len(),"hashes":{"sha256":hex::encode(Sha256::digest(archive))},"custom":{
            "product":PRODUCT,"kind":"model","model_id":c.model_id,"model_version":c.model_version,
            "release_id":sequence,"platform":"windows-x64","channel":"stable","install_scope":"current-user",
            "min_app_protocol":1,"pack_schema":c.pack_schema,"preprocess_version":c.preprocess_version,"worker_protocol":c.worker_protocol,
            "runtime_version":c.runtime_version,"source_revision":c.source_revision,"pack_fingerprint":c.pack_fingerprint,
            "license_policy":c.license_policy,"license_files":c.license_files,"manifest_sha256":c.manifest_sha256,
            "unpacked_length":c.unpacked_length,"rollout_percent":100,"rollout_seed":"model-fixture"}});
        self.rebind();
    }
    fn model_mut(&mut self) -> &mut Value {
        self.targets["targets"]
            .as_object_mut()
            .unwrap()
            .iter_mut()
            .find(|(path, _)| path.starts_with("models/"))
            .unwrap()
            .1
    }
}

#[tokio::test]
async fn independent_signed_model_is_selected_without_changing_application_floor() {
    let mut f = Fixture::new();
    f.add_model(1);
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    let repository = load(&f, &c, &mut state).await.unwrap();
    let app = repository
        .select_app(&c, &mut state, &compatibility())
        .unwrap()
        .unwrap();
    let model = repository
        .select_model(&c, &mut state, &model_compatibility())
        .unwrap()
        .unwrap();
    assert_eq!(app.release().release_id, 71);
    assert_eq!(model.release().release_id, 1);
    assert!(
        model
            .target_name()
            .ends_with("windows-x64/1/model-pack.zip")
    );
    assert!(
        model
            .url()
            .ends_with(&format!("{}.model-pack.zip", hex::encode(model.sha256())))
    );
    assert_eq!(state.state.releases.len(), 2);
    revalidate_model_for_activation(&c, &model, &mut state, &model_compatibility()).unwrap();
    revalidate_for_execution(&c, &app, &mut state).unwrap();
}

#[tokio::test]
async fn model_quarantine_and_health_rollback_do_not_affect_application() {
    let mut f = Fixture::new();
    f.add_model(1);
    let c = f.config();
    let (parent, mut state) = initialized(&c);
    let repository = load(&f, &c, &mut state).await.unwrap();
    let app = repository
        .select_app(&c, &mut state, &compatibility())
        .unwrap()
        .unwrap();
    let model = repository
        .select_model(&c, &mut state, &model_compatibility())
        .unwrap()
        .unwrap();
    state
        .quarantine_model_release(&model.release().model_id, 1)
        .unwrap();
    assert!(
        revalidate_model_for_activation(&c, &model, &mut state, &model_compatibility()).is_err()
    );
    revalidate_for_execution(&c, &app, &mut state).unwrap();
    drop(state);
    let mut state = LockedState::open(&parent.path().join("trust"), &c).unwrap();
    let repository = load(&f, &c, &mut state).await.unwrap();
    assert!(
        repository
            .select_model(&c, &mut state, &model_compatibility())
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .select_app(&c, &mut state, &compatibility())
            .unwrap()
            .is_some()
    );
    assert!(state.quarantine_model_release("unseen-model", 1).is_err());
}

#[tokio::test]
async fn application_quarantine_does_not_block_model_and_model_floor_cannot_authorize_app_quarantine()
 {
    let mut f = Fixture::new();
    f.add_model(500);
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    let repository = load(&f, &c, &mut state).await.unwrap();
    repository
        .select_model(&c, &mut state, &model_compatibility())
        .unwrap();
    assert!(state.quarantine_app_release(400).is_err());
    repository
        .select_app(&c, &mut state, &compatibility())
        .unwrap();
    state.quarantine_app_release(71).unwrap();
    assert!(
        repository
            .select_model(&c, &mut state, &model_compatibility())
            .unwrap()
            .is_some()
    );
    assert!(
        repository
            .select_app(&c, &mut state, &compatibility())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn model_release_replay_and_same_sequence_equivocation_are_rejected() {
    for old_sequence in [1, 2] {
        let mut f = Fixture::new();
        f.add_model(2);
        let c = f.config();
        let (parent, mut state) = initialized(&c);
        load(&f, &c, &mut state)
            .await
            .unwrap()
            .select_model(&c, &mut state, &model_compatibility())
            .unwrap();
        drop(state);
        let mut state = LockedState::open(&parent.path().join("trust"), &c).unwrap();
        f.publish(2, 72);
        f.add_model(old_sequence);
        if old_sequence == 2 {
            f.model_mut()["custom"]["rollout_seed"] = json!("changed");
            f.rebind();
        }
        assert!(matches!(
            load(&f, &c, &mut state).await.unwrap().select_model(
                &c,
                &mut state,
                &model_compatibility()
            ),
            Err(Error::Rollback(_))
        ));
    }
}

#[tokio::test]
async fn signed_model_cannot_broaden_any_compiled_identity_pin() {
    let changes = [
        ("pack_schema", json!(3)),
        ("preprocess_version", json!("new-preprocess")),
        ("worker_protocol", json!(2)),
        ("runtime_version", json!("9.9.9")),
        ("source_revision", json!("unknown-source")),
        ("pack_fingerprint", json!("new-graph-or-DLL")),
        ("license_policy", json!("unreviewed-license")),
        ("license_files", json!(["different-LICENSE"])),
        ("manifest_sha256", json!("c".repeat(64))),
        ("unpacked_length", json!(1025)),
        ("min_app_protocol", json!(2)),
    ];
    for (field, value) in changes {
        let mut f = Fixture::new();
        f.add_model(1);
        let c = f.config();
        let (_p, mut state) = initialized(&c);
        f.model_mut()["custom"][field] = value;
        f.rebind();
        assert!(
            load(&f, &c, &mut state)
                .await
                .unwrap()
                .select_model(&c, &mut state, &model_compatibility())
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn signed_model_approved_stable_and_command_fields_are_rejected() {
    for field in [
        "approved",
        "stable",
        "release_tier",
        "quality_report",
        "command",
        "install_dir",
        "runtime_install",
    ] {
        let mut f = Fixture::new();
        f.add_model(1);
        let c = f.config();
        let (_p, mut state) = initialized(&c);
        f.model_mut()["custom"][field] = json!(true);
        f.rebind();
        assert!(
            load(&f, &c, &mut state)
                .await
                .unwrap()
                .select_model(&c, &mut state, &model_compatibility())
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn model_identity_and_path_are_bound_without_normalization() {
    for mode in [0, 1, 2, 3] {
        let mut f = Fixture::new();
        f.add_model(1);
        let c = f.config();
        let (_p, mut state) = initialized(&c);
        let (name, mut model) = f.targets["targets"]
            .as_object_mut()
            .unwrap()
            .iter()
            .find(|(name, _)| name.starts_with("models/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .unwrap();
        f.targets["targets"].as_object_mut().unwrap().remove(&name);
        let path = match mode {
            0 => {
                model["custom"]["model_version"] = json!("c".repeat(64));
                name.replace(&"a".repeat(64), &"c".repeat(64))
            }
            1 => name.replace("/1/", "/2/"),
            2 => name.replace("model-pack.zip", "../model-pack.zip"),
            _ => {
                model["custom"]["model_id"] = json!("../evil");
                name
            }
        };
        f.targets["targets"][path] = model;
        f.rebind();
        let result = load(&f, &c, &mut state)
            .await
            .and_then(|repository| repository.select_model(&c, &mut state, &model_compatibility()));
        assert!(result.is_err(), "mode {mode}");
    }
}

#[tokio::test]
async fn model_zip_and_expanded_length_limits_fail_closed() {
    for (field, value) in [
        ("length", MODEL_PACKAGE_LIMIT + 1),
        ("unpacked_length", MODEL_UNPACKED_LIMIT + 1),
        ("unpacked_length", 0),
    ] {
        let mut f = Fixture::new();
        f.add_model(1);
        let c = f.config();
        let (_p, mut state) = initialized(&c);
        if field == "length" {
            f.model_mut()[field] = json!(value);
        } else {
            f.model_mut()["custom"][field] = json!(value);
        }
        f.rebind();
        assert!(
            load(&f, &c, &mut state)
                .await
                .unwrap()
                .select_model(&c, &mut state, &model_compatibility())
                .is_err()
        );
    }
}

#[tokio::test]
async fn model_activation_rechecks_policy_metadata_and_installed_version() {
    let mut f = Fixture::new();
    f.add_model(1);
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    let target = load(&f, &c, &mut state)
        .await
        .unwrap()
        .select_model(&c, &mut state, &model_compatibility())
        .unwrap()
        .unwrap();
    let mut changed = model_compatibility();
    changed.pack_fingerprint = "remote-replacement".into();
    assert!(revalidate_model_for_activation(&c, &target, &mut state, &changed).is_err());
    changed = model_compatibility();
    changed.installed_release_id = 1;
    assert!(revalidate_model_for_activation(&c, &target, &mut state, &changed).is_err());
    f.publish(2, 72);
    f.add_model(2);
    load(&f, &c, &mut state).await.unwrap();
    assert!(
        revalidate_model_for_activation(&c, &target, &mut state, &model_compatibility()).is_err()
    );
}

#[tokio::test]
async fn no_model_target_does_not_invent_a_download() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    assert!(
        load(&f, &c, &mut state)
            .await
            .unwrap()
            .select_model(&c, &mut state, &model_compatibility())
            .unwrap()
            .is_none()
    );
}

#[cfg(feature = "fixture-harness")]
#[test]
fn authenticated_fixture_harness_yields_real_model_authority_and_rejects_bad_signatures() {
    let mut f = Fixture::new();
    f.add_model(1);
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    let metadata = f
        .transport
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(path, bytes)| {
            (
                path.trim_start_matches("/metadata/").to_owned(),
                bytes.clone(),
            )
        })
        .collect();
    let repository = crate::fixture::verify_fixture_repository(
        &c,
        &mut state,
        metadata,
        &CancellationToken::new(),
    )
    .unwrap();
    let target = repository
        .select_model(&c, &mut state, &model_compatibility())
        .unwrap()
        .unwrap();
    revalidate_model_for_activation(&c, &target, &mut state, &model_compatibility()).unwrap();
    let mut metadata: BTreeMap<String, Vec<u8>> = f
        .transport
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(path, bytes)| {
            (
                path.trim_start_matches("/metadata/").to_owned(),
                bytes.clone(),
            )
        })
        .collect();
    metadata.insert(
        "timestamp.json".into(),
        signed(&f.timestamp, &[&f.targets_key]),
    );
    assert!(
        crate::fixture::verify_fixture_repository(
            &c,
            &mut state,
            metadata,
            &CancellationToken::new()
        )
        .is_err()
    );
}

#[cfg(feature = "fixture-harness")]
#[test]
fn authenticated_fixture_harness_rejects_arbitrary_paths_and_oversize() {
    let f = Fixture::new();
    let c = f.config();
    let (_p, mut state) = initialized(&c);
    for metadata in [
        BTreeMap::from([("../timestamp.json".into(), vec![1])]),
        BTreeMap::from([(
            "timestamp.json".into(),
            vec![0; METADATA_LIMIT as usize + 1],
        )]),
    ] {
        assert!(
            crate::fixture::verify_fixture_repository(
                &c,
                &mut state,
                metadata,
                &CancellationToken::new()
            )
            .is_err()
        );
    }
}

fn public_bootstrap_manifest(root: &[u8], profile: &str) -> Vec<u8> {
    let root_value: Value = serde_json::from_slice(root).unwrap();
    serde_json::to_vec(&json!({"schema":1,"product":"gloamere-image",
        "platform":"windows-x64","channel":"stable","repository_origin":"https://175.24.190.175",
        "publisher_policy":"tuf-authenticated-unsigned","key_profile":profile,
        "root_version":root_value["signed"]["version"],"root_length":root.len(),
        "root_sha256":hex::encode(Sha256::digest(root))}))
    .unwrap()
}
fn validate_bootstrap(root: &[u8], profile: &str) -> Result<ProductionConfig> {
    ProductionConfig::from_reviewed_public_bundle(&public_bootstrap_manifest(root, profile), root)
}
#[test]
fn public_bootstrap_accepts_both_profiles_but_never_enables_current_build() {
    let mut f = Fixture::new();
    let minimal = f.root_bytes();
    let config = validate_bootstrap(&minimal, "four-role-1-of-1").unwrap();
    assert_eq!(config.origin(), "https://175.24.190.175");
    assert_eq!(config.platform(), "windows-x64");
    assert_eq!(config.channel(), "stable");
    assert_eq!(
        config.publisher_policy(),
        &PublisherPolicy::TufAuthenticatedUnsigned
    );
    let second = TestKey::new();
    let third = TestKey::new();
    for key in [&second, &third] {
        f.root["keys"][&key.id] = key.public.clone();
    }
    f.root["roles"]["root"] = json!({"keyids":[f.root_key.id,second.id,third.id],"threshold":2});
    let recommended = signed(&f.root, &[&f.root_key, &second]);
    validate_bootstrap(&recommended, "root-2-of-3-six-keys").unwrap();
    assert!(validate_bootstrap(&recommended, "four-role-1-of-1").is_err());
    assert!(validate_bootstrap(&signed(&f.root, &[&f.root_key]), "root-2-of-3-six-keys").is_err());
    assert!(matches!(
        ProductionConfig::bundled(),
        Err(Error::NotConfigured(_))
    ));
}
#[test]
fn public_bootstrap_rejects_manifest_scope_policy_and_byte_mismatches() {
    let f = Fixture::new();
    let root = f.root_bytes();
    let good: Value =
        serde_json::from_slice(&public_bootstrap_manifest(&root, "four-role-1-of-1")).unwrap();
    for (field, value) in [
        ("schema", json!(2)),
        ("product", json!("other")),
        ("platform", json!("linux-x64")),
        ("channel", json!("experimental")),
        ("repository_origin", json!("http://175.24.190.175")),
        ("publisher_policy", json!("unsigned")),
        ("key_profile", json!("remote-policy")),
        ("root_version", json!(2)),
        ("root_length", json!(root.len() + 1)),
        ("root_sha256", json!("0".repeat(64))),
        ("private_key", json!("forbidden")),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            ProductionConfig::from_reviewed_public_bundle(
                &serde_json::to_vec(&bad).unwrap(),
                &root
            )
            .is_err(),
            "{field}"
        );
    }
    assert!(ProductionConfig::from_reviewed_public_bundle(b"", &root).is_err());
    assert!(ProductionConfig::from_reviewed_public_bundle(&vec![b' '; 4097], &root).is_err());
    assert!(
        ProductionConfig::from_reviewed_public_bundle(&serde_json::to_vec(&good).unwrap(), b"")
            .is_err()
    );
    let duplicate = String::from_utf8(serde_json::to_vec(&good).unwrap())
        .unwrap()
        .replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
    assert!(ProductionConfig::from_reviewed_public_bundle(duplicate.as_bytes(), &root).is_err());
}
#[test]
fn public_bootstrap_rejects_private_unknown_shared_and_wrong_threshold_root_fields() {
    let f = Fixture::new();
    for case in 0..7 {
        let mut body = f.root.clone();
        match case {
            0 => body["keys"][&f.root_key.id]["keyval"]["private"] = json!("not-a-real-key"),
            1 => body["roles"]["targets"]["keyids"] = json!([f.root_key.id]),
            2 => body["roles"]["root"]["threshold"] = json!(0),
            3 => body["keys"][&f.root_key.id]["keyval"]["public"] = json!("0".repeat(64)),
            4 => body["roles"]["targets"]["command"] = json!("forbidden"),
            5 => body["private_key"] = json!("forbidden"),
            _ => body["consistent_snapshot"] = json!(false),
        }
        assert!(
            validate_bootstrap(&signed(&body, &[&f.root_key]), "four-role-1-of-1").is_err(),
            "case {case}"
        );
    }
    let mut envelope: Value = serde_json::from_slice(&f.root_bytes()).unwrap();
    envelope["signatures"][0]["sig"] = json!("00".repeat(64));
    assert!(
        validate_bootstrap(&serde_json::to_vec(&envelope).unwrap(), "four-role-1-of-1").is_err()
    );
}
#[test]
fn public_bootstrap_rejects_duplicate_role_key_and_signature_entries() {
    let f = Fixture::new();
    let root = String::from_utf8(f.root_bytes()).unwrap();
    let key = serde_json::to_string(&f.root_key.public).unwrap();
    let duplicate_key = root.replacen(
        "\"keys\":{",
        &format!("\"keys\":{{\"{}\":{},", f.root_key.id, key),
        1,
    );
    assert!(validate_bootstrap(duplicate_key.as_bytes(), "four-role-1-of-1").is_err());
    let duplicate_role = root.replacen(
        "\"roles\":{",
        &format!(
            "\"roles\":{{\"root\":{},",
            serde_json::to_string(&f.root["roles"]["root"]).unwrap()
        ),
        1,
    );
    assert!(validate_bootstrap(duplicate_role.as_bytes(), "four-role-1-of-1").is_err());
    let mut envelope: Value = serde_json::from_slice(root.as_bytes()).unwrap();
    let duplicate = envelope["signatures"][0].clone();
    envelope["signatures"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(
        validate_bootstrap(&serde_json::to_vec(&envelope).unwrap(), "four-role-1-of-1").is_err()
    );
}
#[test]
fn public_bootstrap_rejects_future_expired_excessive_lifetime_and_state_reset() {
    let f = Fixture::new();
    for (issued, expires) in [
        (f.now + 86400, f.now + 2 * 86400),
        (f.now - 86400, f.now - 1),
        (f.now, f.now + 367 * 86400),
    ] {
        let mut body = f.root.clone();
        body["issued"] = json!(ts(issued));
        body["expires"] = json!(ts(expires));
        assert!(validate_bootstrap(&signed(&body, &[&f.root_key]), "four-role-1-of-1").is_err());
    }
    let config = validate_bootstrap(&f.root_bytes(), "four-role-1-of-1").unwrap();
    let (dir, state) = initialized(&config);
    drop(state);
    let other = Fixture::new();
    let other_config = validate_bootstrap(&other.root_bytes(), "four-role-1-of-1").unwrap();
    assert!(LockedState::open(&dir.path().join("trust"), &config).is_ok());
    assert!(LockedState::open(&dir.path().join("trust"), &other_config).is_err());
}

fn expired_bootstrap_fixture() -> (Fixture, ProductionConfig) {
    let mut f = Fixture::new();
    f.root["issued"] = json!(ts(f.now - 3 * 86400));
    f.root["expires"] = json!(ts(f.now - 86400));
    let anchor = f.root_bytes();
    let manifest = public_bootstrap_manifest(&anchor, "four-role-1-of-1");
    assert!(ProductionConfig::from_reviewed_public_bundle(&manifest, &anchor).is_err());
    let config = crate::bootstrap::from_compiled_public_anchor(&manifest, &anchor).unwrap();
    (f, config)
}
fn root_successor(f: &Fixture, new_key: &TestKey, expired: bool) -> Value {
    let mut next = f.root.clone();
    next["version"] = json!(2);
    next["issued"] = json!(ts(if expired { f.now - 2 * 86400 } else { f.now }));
    next["expires"] = json!(ts(if expired {
        f.now - 86400
    } else {
        f.now + 30 * 86400
    }));
    next["keys"].as_object_mut().unwrap().remove(&f.root_key.id);
    next["keys"][&new_key.id] = new_key.public.clone();
    next["roles"]["root"] = json!({"keyids":[new_key.id],"threshold":1});
    next
}
#[tokio::test]
async fn expired_compiled_anchor_rotates_and_reopens_with_saved_valid_root() {
    let (f, config) = expired_bootstrap_fixture();
    let new_key = TestKey::new();
    let next = root_successor(&f, &new_key, false);
    f.transport
        .put("2.root.json", signed(&next, &[&f.root_key, &new_key]));
    let (dir, mut state) = initialized(&config);
    let repo = load(&f, &config, &mut state).await.unwrap();
    assert!(
        repo.select_app(&config, &mut state, &compatibility())
            .unwrap()
            .is_some()
    );
    assert_eq!(state.snapshot().highest_metadata["root"].version, 2);
    drop(state);
    // Offline saved root evidence is enough to bootstrap the next refresh;
    // the server no longer has to supply the already persisted rotation.
    f.transport
        .0
        .lock()
        .unwrap()
        .remove("/metadata/2.root.json");
    let mut reopened = LockedState::open(&dir.path().join("trust"), &config).unwrap();
    load(&f, &config, &mut reopened).await.unwrap();
    assert_eq!(reopened.snapshot().highest_metadata["root"].version, 2);
}
#[tokio::test]
async fn expired_compiled_anchor_without_valid_fresh_rotation_fails_closed() {
    for case in 0..3 {
        let (f, config) = expired_bootstrap_fixture();
        let key = TestKey::new();
        if case != 0 {
            let next = root_successor(&f, &key, case == 2);
            let bytes = if case == 1 {
                signed(&next, &[&key])
            } else {
                signed(&next, &[&f.root_key, &key])
            };
            f.transport.put("2.root.json", bytes);
        }
        let (_dir, mut state) = initialized(&config);
        assert!(load(&f, &config, &mut state).await.is_err(), "case {case}");
        assert!(state.snapshot().releases.is_empty());
    }
}
#[tokio::test]
async fn expired_compiled_anchor_rotation_preserves_metadata_and_release_floors() {
    let (mut f, config) = expired_bootstrap_fixture();
    let key = TestKey::new();
    let next = root_successor(&f, &key, false);
    f.transport
        .put("2.root.json", signed(&next, &[&f.root_key, &key]));
    let (dir, mut state) = initialized(&config);
    f.publish(2, 72);
    load(&f, &config, &mut state)
        .await
        .unwrap()
        .select_app(&config, &mut state, &compatibility())
        .unwrap()
        .unwrap();
    drop(state);
    let mut state = LockedState::open(&dir.path().join("trust"), &config).unwrap();
    f.publish(1, 71);
    assert!(load(&f, &config, &mut state).await.is_err());
    assert_eq!(state.snapshot().highest_metadata["root"].version, 2);
    assert_eq!(state.snapshot().highest_metadata["timestamp"].version, 2);
    assert_eq!(
        state.snapshot().releases.values().next().unwrap().sequence,
        72
    );
    f.publish(3, 71);
    let repo = load(&f, &config, &mut state).await.unwrap();
    assert!(
        repo.select_app(&config, &mut state, &compatibility())
            .is_err()
    );
    assert_eq!(
        state.snapshot().releases.values().next().unwrap().sequence,
        72
    );
}

#[tokio::test]
async fn legacy_noncanonical_identity_is_retained_without_reset_or_floor_reduction() {
    let f = Fixture::new();
    let config = f.config();
    let (directory, mut state) = initialized(&config);
    let repository = load(&f, &config, &mut state).await.unwrap();
    let (name, target) = repository
        .repository
        .targets()
        .signed
        .targets
        .iter()
        .next()
        .unwrap();
    // Model the unpublished preserve_order serialization before canonicalization.
    let legacy_bytes = serde_json::to_vec(&(name.raw(), target)).unwrap();
    let legacy_identity = hex::encode(Sha256::digest(legacy_bytes));
    let canonical = crate::state::signed_target_identity(name.raw(), target).unwrap();
    assert_ne!(legacy_identity, canonical);
    let scope = format!("{PRODUCT}/{}/{}/app", config.platform(), config.channel());
    let mut next = state.state.clone();
    next.releases.insert(
        scope.clone(),
        crate::state::ReleaseFloor {
            sequence: 71,
            identity: legacy_identity.clone(),
        },
    );
    state.commit(next).unwrap();
    let trust = directory.path().join("trust");
    drop(state);
    let mut state = LockedState::open(&trust, &config).unwrap();
    let repository = load(&f, &config, &mut state).await.unwrap();
    let before = std::fs::read(trust.join("trusted-state-v1.json")).unwrap();
    assert!(matches!(
        repository.select_app(&config, &mut state, &compatibility()),
        Err(Error::Rollback(_))
    ));
    let mut lowered = state.state.clone();
    lowered.releases.get_mut(&scope).unwrap().sequence = 70;
    assert!(matches!(state.commit(lowered), Err(Error::Rollback(_))));
    let mut removed = state.state.clone();
    removed.releases.remove(&scope);
    assert!(matches!(state.commit(removed), Err(Error::Rollback(_))));
    assert_eq!(
        std::fs::read(trust.join("trusted-state-v1.json")).unwrap(),
        before
    );
    drop(state);
    let state = LockedState::open(&trust, &config).unwrap();
    assert_eq!(state.snapshot().releases[&scope].sequence, 71);
    assert_eq!(state.snapshot().releases[&scope].identity, legacy_identity);
}
