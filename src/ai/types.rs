//! Portable AI evidence snapshots. These records never change image files.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIdentity {
    /// A filesystem-stable ID when available; otherwise the canonical path.
    pub key: String,
    /// Full-file SHA-256, verified for the current content. Never size/mtime alone.
    pub content_hash: String,
    pub size: u64,
    pub modified_ns: u128,
}

impl AssetIdentity {
    pub fn has_content_hash(&self) -> bool {
        is_content_hash(&self.content_hash)
    }

    /// Rename and timestamp changes do not invalidate a verified content match.
    pub fn same_content(&self, other: &Self) -> bool {
        !self.key.is_empty()
            && self.key == other.key
            && self.has_content_hash()
            && self.content_hash == other.content_hash
    }
}

pub fn is_content_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    TrustedGeneration,
    UnsignedGeneration,
    Capture,
    CompatibleMarker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub provider: String,
    pub summary: String,
    /// True only after signature, current-content binding AND trust policy pass.
    /// A compatible marker remains a marker; an authenticated generation claim
    /// must be emitted separately as TrustedGeneration.
    pub authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResult {
    /// Package identity covering weights, head, runtime and preprocessing.
    pub fingerprint: String,
    pub input_scope: String,
    /// A detector score, not a probability of generation or scene truth.
    pub raw_score: f64,
    pub threshold: f64,
    /// Set only by the application's shipped approval policy, never an import.
    pub approved: bool,
    pub high_signal: bool,
}

impl ModelResult {
    pub fn valid(&self) -> bool {
        !self.fingerprint.is_empty()
            && !self.input_scope.is_empty()
            && self.raw_score.is_finite()
            && self.threshold.is_finite()
    }

    pub fn usable_for_automatic_decision(&self) -> bool {
        self.approved && self.valid()
    }

    pub fn is_automatic_high_signal(&self) -> bool {
        self.usable_for_automatic_decision() && self.high_signal && self.raw_score >= self.threshold
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unscanned,
    Queued,
    Running,
    Paused,
    AwaitingModel,
    Unsupported(String),
    Complete,
    Failed(String),
    TimedOut,
}

impl Phase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unscanned => "待检测",
            Self::Queued => "已排队",
            Self::Running => "检测中",
            Self::Paused => "已暂停",
            Self::AwaitingModel => "仅完成来源检查",
            Self::Unsupported(_) => "暂不支持",
            Self::Complete => "检查完成",
            Self::Failed(_) => "检测失败",
            Self::TimedOut => "检测超时",
        }
    }

    pub fn is_pending(&self) -> bool {
        !matches!(
            self,
            Self::Complete | Self::Unsupported(_) | Self::Failed(_) | Self::TimedOut
        )
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::TimedOut)
    }
}

/// Component integrity/readiness is independent of a particular image's task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    #[default]
    Missing,
    Checking,
    Ready,
    Failed(String),
}
impl ComponentState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Missing => "未安装像素检测组件",
            Self::Checking => "正在校验像素检测组件",
            Self::Ready => "像素检测组件已就绪",
            Self::Failed(_) => "像素检测组件校验失败",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTier {
    #[default]
    Unavailable,
    Experimental,
    Stable,
}
impl ReleaseTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "无可用像素组件",
            Self::Experimental => "实验组件 · 尚未正式验证",
            Self::Stable => "稳定组件",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Unscanned,
    SourceOnly,
    SourceQueued,
    SourceRunning,
    SourceFailed(String),
    Disabled,
    Queued,
    Running,
    Paused,
    CompletedExperimental,
    CompletedStable,
    Unsupported(String),
    Expired,
    Failed(String),
    TimedOut,
}
impl RunState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unscanned => "尚未检查",
            Self::SourceOnly => "仅完成来源检查",
            Self::SourceQueued => "等待来源检查",
            Self::SourceRunning => "正在来源检查",
            Self::SourceFailed(_) => "来源检查失败",
            Self::Disabled => "像素检测未启用",
            Self::Queued => "等待检测",
            Self::Running => "正在检测",
            Self::Paused => "检测已暂停",
            Self::CompletedExperimental => "实验检测已完成",
            Self::CompletedStable => "像素检测已完成",
            Self::Unsupported(_) => "此格式暂不支持像素检测",
            Self::Expired => "检测结果已过期",
            Self::Failed(_) => "像素检测失败",
            Self::TimedOut => "像素检测超时",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueCause {
    UserManual,
    ExperimentalFolder,
    StableAutomatic,
    SourceOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub identity: AssetIdentity,
    pub evidence: Vec<Evidence>,
    pub phase: Phase,
    pub model: Option<ModelResult>,
    pub checked_unix_seconds: u64,
    /// Additive v0.7 fields; legacy phase stays v0.6-deserializable for rollback.
    #[serde(default)]
    pub component_state: ComponentState,
    #[serde(default)]
    pub run_state: Option<RunState>,
    #[serde(default)]
    pub release_tier: ReleaseTier,
    #[serde(default)]
    pub source_error: Option<String>,
    #[serde(default)]
    pub pixel_checked_unix_seconds: Option<u64>,
    /// Signed package revision is independent from the model quality fingerprint.
    #[serde(default)]
    pub pack_revision: Option<String>,
}

impl MachineRecord {
    pub fn unscanned(identity: AssetIdentity) -> Self {
        Self {
            identity,
            evidence: Vec::new(),
            phase: Phase::Unscanned,
            model: None,
            checked_unix_seconds: 0,
            component_state: ComponentState::Missing,
            run_state: Some(RunState::Unscanned),
            release_tier: ReleaseTier::Unavailable,
            source_error: None,
            pixel_checked_unix_seconds: None,
            pack_revision: None,
        }
    }

    /// Invalidate only model output, retaining independent evidence and manual
    /// records. A changed threshold can instead be updated and policy rerun.
    pub fn invalidate_model(&mut self, expected_fingerprint: &str) -> bool {
        if self
            .model
            .as_ref()
            .is_some_and(|m| m.fingerprint != expected_fingerprint)
        {
            self.model = None;
            self.run_state = Some(RunState::Expired);
            if self.phase == Phase::Complete {
                self.phase = Phase::AwaitingModel;
            }
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualDecision {
    Ai,
    NotAi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualOverride {
    pub content_hash: String,
    pub decision: ManualDecision,
    pub created_unix_seconds: u64,
}

impl ManualOverride {
    pub fn is_active_for(&self, identity: &AssetIdentity) -> bool {
        identity.has_content_hash() && self.content_hash == identity.content_hash
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Ai,
    Unmarked,
    Pending,
    Error,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub bucket: Bucket,
    pub label: String,
    pub badge: Option<String>,
    pub conflict: bool,
    pub manual_active: bool,
    /// True for the effective automatic suspected label, not manual/source AI.
    pub suspected: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    #[default]
    All,
    Ai,
    Unmarked,
    Pending,
    Error,
    Manual,
    SourceEvidence,
    Suspected,
    NoSignal,
    Unsupported,
}

impl Filter {
    pub const ALL: [Self; 10] = [
        Self::All,
        Self::Ai,
        Self::Unmarked,
        Self::Pending,
        Self::Error,
        Self::Manual,
        Self::SourceEvidence,
        Self::Suspected,
        Self::NoSignal,
        Self::Unsupported,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Ai => "AI",
            Self::Unmarked => "未标记 AI",
            Self::Pending => "未完成",
            Self::Error => "检测异常",
            Self::Manual => "人工标记",
            Self::SourceEvidence => "来源证据 AI",
            Self::Suspected => "疑似 AI",
            Self::NoSignal => "未见强信号",
            Self::Unsupported => "不支持",
        }
    }

    pub fn matches(self, decision: &Decision, include_suspected: bool) -> bool {
        match self {
            Self::All => true,
            Self::Ai => decision.bucket == Bucket::Ai && (include_suspected || !decision.suspected),
            Self::Unmarked => decision.bucket == Bucket::Unmarked,
            Self::Pending => decision.bucket == Bucket::Pending,
            Self::Error => decision.bucket == Bucket::Error,
            Self::Manual => decision.manual_active,
            Self::SourceEvidence => decision.label == "来源证据 AI",
            Self::Suspected => decision.suspected,
            Self::NoSignal => decision.bucket == Bucket::Unmarked && !decision.manual_active,
            Self::Unsupported => decision.bucket == Bucket::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterCounts {
    pub all: usize,
    pub ai: usize,
    pub unmarked: usize,
    pub pending: usize,
    pub error: usize,
    pub manual: usize,
    pub suspected: usize,
    pub source: usize,
    pub no_signal: usize,
    pub unsupported: usize,
}

impl FilterCounts {
    pub fn add(&mut self, decision: &Decision) {
        self.all += 1;
        match decision.bucket {
            Bucket::Ai => self.ai += 1,
            Bucket::Unmarked => self.unmarked += 1,
            Bucket::Pending => self.pending += 1,
            Bucket::Error => self.error += 1,
            Bucket::Unsupported => self.unsupported += 1,
        }
        self.source += usize::from(decision.label == "来源证据 AI");
        self.no_signal +=
            usize::from(decision.bucket == Bucket::Unmarked && !decision.manual_active);
        self.manual += usize::from(decision.manual_active);
        self.suspected += usize::from(decision.bucket == Bucket::Ai && decision.suspected);
    }

    pub fn count(self, filter: Filter) -> usize {
        match filter {
            Filter::All => self.all,
            Filter::Ai => self.ai,
            Filter::Unmarked => self.unmarked,
            Filter::Pending => self.pending,
            Filter::Error => self.error,
            Filter::Manual => self.manual,
            Filter::SourceEvidence => self.source,
            Filter::Suspected => self.suspected,
            Filter::NoSignal => self.no_signal,
            Filter::Unsupported => self.unsupported,
        }
    }

    pub fn visible_count(self, filter: Filter, include_suspected: bool) -> usize {
        if filter == Filter::Ai && !include_suspected {
            self.ai.saturating_sub(self.suspected)
        } else {
            self.count(filter)
        }
    }
}
