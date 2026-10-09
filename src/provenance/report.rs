use serde::{Deserialize, Serialize};

pub const POLICY_VERSION: &str = "offline-sdk0912-v1";
pub const TRUST_SNAPSHOT: &str = "none-v1";
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceState {
    NoCredential,
    Signed,
    Incomplete,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckState {
    Passed,
    Failed,
    NotChecked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataField {
    pub label: String,
    pub value: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceReport {
    pub file_sha256: String,
    pub policy_version: String,
    pub trust_snapshot: String,
    pub checked_unix_seconds: u64,
    pub state: EvidenceState,
    pub signature: CheckState,
    pub asset_binding: CheckState,
    pub trust: String,
    pub offline_limitations: Vec<String>,
    pub camera_exif: Vec<MetadataField>,
    pub unsigned_claims: Vec<MetadataField>,
    pub signed_declarations: Vec<MetadataField>,
    pub status_codes: Vec<String>,
    pub details: String,
    pub blocked_http_attempts: u64,
}
impl EvidenceReport {
    pub fn incomplete(hash: String, detail: impl Into<String>) -> Self {
        Self {
            file_sha256: hash,
            policy_version: POLICY_VERSION.into(),
            trust_snapshot: TRUST_SNAPSHOT.into(),
            checked_unix_seconds: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            state: EvidenceState::Incomplete,
            signature: CheckState::NotChecked,
            asset_binding: CheckState::NotChecked,
            trust: "未配置信任快照".into(),
            offline_limitations: vec!["离线模式：未联网补全 manifest、信任列表或 OCSP".into()],
            camera_exif: Vec::new(),
            unsigned_claims: Vec::new(),
            signed_declarations: Vec::new(),
            status_codes: Vec::new(),
            details: detail.into(),
            blocked_http_attempts: 0,
        }
    }
    pub fn key(&self) -> (String, String, String) {
        (
            self.file_sha256.clone(),
            self.policy_version.clone(),
            self.trust_snapshot.clone(),
        )
    }
}
