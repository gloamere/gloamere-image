//! Shared, side-effect-free evidence policy. Absence of AI evidence never proves
//! that an image is a real photograph or that its depicted events happened.
pub use super::types::{Bucket, Decision, Filter, FilterCounts};
use super::types::{EvidenceKind, MachineRecord, ManualDecision, ManualOverride, Phase, RunState};

pub const POLICY_VERSION: u32 = 2;

pub fn decide(record: Option<&MachineRecord>, manual: Option<&ManualOverride>) -> Decision {
    let mut reasons = Vec::new();
    let mut trusted_generation = false;
    let mut unsigned_generation = false;
    let mut capture = false;
    let mut model_high = false;
    let mut experimental_score = None;

    if let Some(record) = record {
        if let Some(error) = &record.source_error {
            reasons.push(format!("来源检查异常（与像素检测独立）：{error}"));
        }
        for evidence in &record.evidence {
            match evidence.kind {
                EvidenceKind::TrustedGeneration if evidence.authenticated => {
                    trusted_generation = true;
                    reasons.push(format!(
                        "受信生成来源：{}；{}",
                        evidence.provider, evidence.summary
                    ));
                }
                EvidenceKind::TrustedGeneration => {
                    reasons.push(format!(
                        "生成来源声明未通过认证：{}；{}",
                        evidence.provider, evidence.summary
                    ));
                }
                EvidenceKind::UnsignedGeneration => {
                    unsigned_generation = true;
                    reasons.push(format!(
                        "非签名生成元数据（可被修改）：{}；{}",
                        evidence.provider, evidence.summary
                    ));
                }
                EvidenceKind::Capture => {
                    capture = true;
                    reasons.push(format!(
                        "采集来源记录：{}；{}；不能排除翻拍或编辑",
                        evidence.provider, evidence.summary
                    ));
                }
                EvidenceKind::CompatibleMarker => {
                    reasons.push(format!(
                        "检测到兼容标记：{}；{}；仅此不足以证明生成来源",
                        evidence.provider, evidence.summary
                    ));
                }
            }
        }
        if let Some(model) = &record.model {
            reasons.push(format!(
                "像素检测分值 {}；{}；输入范围：{}。分值不是概率。",
                model.raw_score,
                if model.approved {
                    format!("发布阈值 {}", model.threshold)
                } else {
                    "未设置发布阈值；上游零分界仅用于研究".into()
                },
                model.input_scope
            ));
            if !model.approved {
                reasons.push(
                    "模型尚未通过正式发布的验证标准；实验推断可能误报、漏报，不能用于确认图片来源"
                        .into(),
                );
                if record.run_state == Some(RunState::CompletedExperimental)
                    && record.phase == Phase::Complete
                    && record.release_tier == super::types::ReleaseTier::Experimental
                    && record.identity.has_content_hash()
                    && model.valid()
                    && model.fingerprint == super::model::fingerprint()
                    && model.threshold == super::model::RESEARCH_THRESHOLD
                {
                    experimental_score = Some(model.raw_score);
                }
            } else if !model.valid() {
                reasons.push("模型输出无效，不能用于自动判定".into());
            } else if matches!(record.phase, Phase::Unsupported(_))
                || record.run_state == Some(RunState::Expired)
            {
                reasons.push("输入不在支持范围，模型分值不参与自动分类".into());
            } else {
                model_high = model.is_automatic_high_signal();
                if !model_high {
                    reasons.push("模型未给出适用的强信号；这不能否定其他来源证据".into());
                }
            }
        }
        match &record.phase {
            Phase::Failed(reason) => reasons.push(format!("检查失败：{reason}")),
            Phase::TimedOut => reasons.push("检查超时，可重试；不等同于没有 AI 信号".into()),
            Phase::Unsupported(reason) => reasons.push(format!("暂不支持：{reason}")),
            Phase::Paused => reasons.push("后台检查已暂停".into()),
            _ => {}
        }
    }

    let experimental_high =
        experimental_score.is_some_and(|score| score >= super::model::RESEARCH_THRESHOLD);
    let positive = trusted_generation || unsigned_generation || model_high || experimental_high;
    let evidence_conflict = positive && capture;
    if evidence_conflict {
        reasons.push("生成信号与采集来源记录并存：可能涉及翻拍、编辑或误报；两者均保留".into());
    }

    if let Some(manual) = manual {
        if record.is_some_and(|r| manual.is_active_for(&r.identity)) {
            let conflict =
                evidence_conflict || (manual.decision == ManualDecision::NotAi && positive);
            if manual.decision == ManualDecision::NotAi && positive {
                reasons.push("人工非 AI 决定与自动生成证据冲突；自动依据仍保留".into());
            }
            reasons.push("人工决定仅影响本地整理，不代表真实性已验证".into());
            let (bucket, label, badge) = match manual.decision {
                ManualDecision::Ai => (Bucket::Ai, "人工标记 AI", Some("AI · 手动")),
                ManualDecision::NotAi => (Bucket::Unmarked, "人工标记非 AI", Some("非 AI · 手动")),
            };
            return decision(bucket, label, badge, conflict, true, false, reasons);
        }
        reasons.push("原人工标记待重新确认：图片内容已变化或当前内容指纹尚未核验".into());
    }

    if trusted_generation {
        return decision(
            Bucket::Ai,
            "来源证据 AI",
            Some("AI"),
            evidence_conflict,
            false,
            false,
            reasons,
        );
    }
    if unsigned_generation || model_high {
        return decision(
            Bucket::Ai,
            "疑似 AI",
            Some("AI ?"),
            evidence_conflict,
            false,
            true,
            reasons,
        );
    }
    if let Some(score) = experimental_score {
        reasons.push(
            "实验检测已完成；只覆盖原图中央 224×224，分值和零分界均不是已校准的概率或正式阈值"
                .into(),
        );
        if score >= super::model::RESEARCH_THRESHOLD {
            return decision(
                Bucket::Ai,
                "疑似 AI · 实验",
                Some("AI ? · 实验"),
                evidence_conflict,
                false,
                true,
                reasons,
            );
        }
        reasons.push("不能排除 AI；未见明显信号不等于已证实非 AI".into());
        return decision(
            Bucket::Unmarked,
            "未发现明显 AI 信号 · 实验",
            None,
            evidence_conflict,
            false,
            false,
            reasons,
        );
    }
    let Some(record) = record else {
        return decision(
            Bucket::Pending,
            "待检测",
            None,
            false,
            false,
            false,
            reasons,
        );
    };
    if record.run_state == Some(RunState::Expired) {
        reasons.push(
            "文件内容或模型、预处理、运行时协议、研究策略已变化；重新检测前不沿用旧分值".into(),
        );
        return decision(
            Bucket::Pending,
            "检测结果已过期",
            None,
            false,
            false,
            false,
            reasons,
        );
    }
    if matches!(record.phase, Phase::Unsupported(_)) {
        return decision(
            Bucket::Unsupported,
            "此格式暂不支持像素检测",
            None,
            false,
            false,
            false,
            reasons,
        );
    }
    if record.phase.is_error() {
        return decision(
            Bucket::Error,
            record
                .run_state
                .as_ref()
                .map(RunState::label)
                .unwrap_or(record.phase.label()),
            None,
            false,
            false,
            false,
            reasons,
        );
    }
    if record.phase.is_pending() {
        if record.phase == Phase::AwaitingModel {
            reasons.push("仅完成来源检查；没有运行像素检测，不能排除 AI".into());
        }
        return decision(
            Bucket::Pending,
            record
                .run_state
                .as_ref()
                .map(RunState::label)
                .unwrap_or("仅完成来源检查"),
            None,
            false,
            false,
            false,
            reasons,
        );
    }
    // A source-only scan cannot manufacture completion of a general AI check.
    match record.model.as_ref() {
        None => {
            reasons.push("尚未启用像素检测；未发现来源标记不等于非 AI".into());
            decision(
                Bucket::Pending,
                "仅完成来源检查",
                None,
                false,
                false,
                false,
                reasons,
            )
        }
        Some(model) if !model.approved => decision(
            Bucket::Pending,
            "仅完成来源检查",
            None,
            false,
            false,
            false,
            reasons,
        ),
        Some(model) if !model.valid() => decision(
            Bucket::Error,
            "模型输出无效",
            None,
            false,
            false,
            false,
            reasons,
        ),
        Some(_) => {
            reasons.push("未标记 AI 不等于已证实非 AI，也不证明画面内容真实".into());
            decision(
                Bucket::Unmarked,
                "未发现强信号",
                None,
                false,
                false,
                false,
                reasons,
            )
        }
    }
}

fn decision(
    bucket: Bucket,
    label: &str,
    badge: Option<&str>,
    conflict: bool,
    manual_active: bool,
    suspected: bool,
    reasons: Vec<String>,
) -> Decision {
    Decision {
        bucket,
        label: label.into(),
        badge: badge.map(str::to_owned),
        conflict,
        manual_active,
        suspected,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::{AssetIdentity, Evidence, ModelResult};

    fn record(phase: Phase) -> MachineRecord {
        MachineRecord {
            identity: AssetIdentity {
                key: "file:1".into(),
                content_hash: "a".repeat(64),
                size: 7,
                modified_ns: 1,
            },
            evidence: Vec::new(),
            phase,
            model: None,
            checked_unix_seconds: 0,
            component_state: Default::default(),
            run_state: None,
            release_tier: Default::default(),
            source_error: None,
            pixel_checked_unix_seconds: None,
            pack_revision: None,
        }
    }

    fn evidence(kind: EvidenceKind, authenticated: bool) -> Evidence {
        Evidence {
            kind,
            provider: "test".into(),
            summary: "claim".into(),
            authenticated,
        }
    }

    fn model(approved: bool, high: bool) -> ModelResult {
        ModelResult {
            fingerprint: "approved:test".into(),
            input_scope: "center crop".into(),
            raw_score: if high { 0.9 } else { 0.1 },
            threshold: 0.8,
            approved,
            high_signal: high,
        }
    }

    fn manual(decision: ManualDecision) -> ManualOverride {
        ManualOverride {
            content_hash: "a".repeat(64),
            decision,
            created_unix_seconds: 1,
        }
    }

    #[test]
    fn required_conflict_cases_preserve_evidence() {
        let mut r = record(Phase::Complete);
        r.model = Some(model(true, true));
        r.evidence.push(evidence(EvidenceKind::Capture, true));
        let d = decide(Some(&r), None);
        assert_eq!(d.bucket, Bucket::Ai);
        assert!(d.suspected && d.conflict);
        assert_eq!(r.evidence.len(), 1);
        r.model = Some(model(true, false));
        r.evidence
            .push(evidence(EvidenceKind::TrustedGeneration, true));
        let d = decide(Some(&r), None);
        assert_eq!(d.label, "来源证据 AI");
        assert!(!d.suspected);
        let d = decide(Some(&r), Some(&manual(ManualDecision::NotAi)));
        assert_eq!(d.bucket, Bucket::Unmarked);
        assert!(d.manual_active && d.conflict);
        assert_eq!(r.evidence.len(), 2);
    }

    #[test]
    fn unsigned_and_untrusted_claims_never_become_trusted() {
        let mut r = record(Phase::Failed("signature invalid".into()));
        r.evidence
            .push(evidence(EvidenceKind::TrustedGeneration, false));
        assert_eq!(decide(Some(&r), None).bucket, Bucket::Error);
        r.evidence
            .push(evidence(EvidenceKind::UnsignedGeneration, false));
        let d = decide(Some(&r), None);
        assert!(d.suspected);
        assert_eq!(d.badge.as_deref(), Some("AI ?"));
        assert!(d.reasons.iter().any(|s| s.contains("signature invalid")));
    }

    #[test]
    fn compatible_markers_do_not_claim_generation_even_when_authenticated() {
        for authenticated in [false, true] {
            let mut r = record(Phase::Complete);
            r.evidence
                .push(evidence(EvidenceKind::CompatibleMarker, authenticated));
            assert_eq!(decide(Some(&r), None).bucket, Bucket::Pending);
        }
    }

    #[test]
    fn absent_and_unapproved_models_stay_pending() {
        for phase in [Phase::Complete, Phase::AwaitingModel] {
            let mut r = record(phase);
            assert_eq!(decide(Some(&r), None).bucket, Bucket::Pending);
            r.model = Some(model(false, true));
            assert_eq!(decide(Some(&r), None).bucket, Bucket::Pending);
        }
    }

    #[test]
    fn stale_manual_never_applies_to_changed_or_unverified_content() {
        let mut r = record(Phase::Unscanned);
        let m = manual(ManualDecision::Ai);
        assert!(decide(Some(&r), Some(&m)).manual_active);
        r.identity.content_hash = "b".repeat(64);
        let d = decide(Some(&r), Some(&m));
        assert_eq!(d.bucket, Bucket::Pending);
        assert!(!d.manual_active);
        assert!(d.reasons.iter().any(|s| s.contains("待重新确认")));
        assert!(!decide(None, Some(&m)).manual_active);
        r.identity.content_hash.clear();
        assert!(!decide(Some(&r), Some(&m)).manual_active);
    }

    #[test]
    fn each_phase_has_correct_primary_bucket_and_positive_precedence() {
        let phases = [
            Phase::Unscanned,
            Phase::Queued,
            Phase::Running,
            Phase::Paused,
            Phase::AwaitingModel,
            Phase::Unsupported("animation".into()),
            Phase::Complete,
            Phase::Failed("parse".into()),
            Phase::TimedOut,
        ];
        for phase in phases {
            let mut r = record(phase.clone());
            assert_eq!(
                decide(Some(&r), None).bucket,
                if phase.is_error() {
                    Bucket::Error
                } else if matches!(phase, Phase::Unsupported(_)) {
                    Bucket::Unsupported
                } else {
                    Bucket::Pending
                }
            );
            r.evidence
                .push(evidence(EvidenceKind::TrustedGeneration, true));
            assert_eq!(decide(Some(&r), None).bucket, Bucket::Ai);
            assert_eq!(
                decide(Some(&r), Some(&manual(ManualDecision::NotAi))).bucket,
                Bucket::Unmarked
            );
        }
    }

    #[test]
    fn invalid_out_of_scope_or_below_threshold_scores_never_classify_ai() {
        let mut r = record(Phase::Complete);
        for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.1] {
            let mut m = model(true, true);
            m.raw_score = score;
            r.model = Some(m);
            assert_ne!(decide(Some(&r), None).bucket, Bucket::Ai);
        }
        r.model = Some(model(true, true));
        r.phase = Phase::Unsupported("outside validated scope".into());
        assert_eq!(decide(Some(&r), None).bucket, Bucket::Unsupported);
    }

    #[test]
    fn approved_low_signal_is_unmarked_not_a_truth_claim() {
        let mut r = record(Phase::Complete);
        r.model = Some(model(true, false));
        let d = decide(Some(&r), None);
        assert_eq!(d.bucket, Bucket::Unmarked);
        assert_eq!(d.label, "未发现强信号");
        assert!(d.badge.is_none());
    }

    #[test]
    fn counts_are_exhaustive_and_manual_is_an_overlapping_view() {
        let mut records = [
            record(Phase::Unscanned),
            record(Phase::Failed("bad".into())),
            record(Phase::Complete),
            record(Phase::Complete),
        ];
        records[2].model = Some(model(true, false));
        records[3]
            .evidence
            .push(evidence(EvidenceKind::UnsignedGeneration, false));
        let mut decisions: Vec<_> = records.iter().map(|r| decide(Some(r), None)).collect();
        decisions.push(decide(Some(&records[0]), Some(&manual(ManualDecision::Ai))));
        decisions.push(decide(
            Some(&records[0]),
            Some(&manual(ManualDecision::NotAi)),
        ));
        let mut counts = FilterCounts::default();
        for d in &decisions {
            counts.add(d);
        }
        assert_eq!(
            counts.all,
            counts.ai + counts.unmarked + counts.pending + counts.error
        );
        assert_eq!(counts.manual, 2);
        assert_eq!(counts.visible_count(Filter::Ai, false), 1);
        for filter in Filter::ALL {
            assert_eq!(
                counts.count(filter),
                decisions.iter().filter(|d| filter.matches(d, true)).count()
            );
            assert_eq!(
                counts.visible_count(filter, false),
                decisions
                    .iter()
                    .filter(|d| filter.matches(d, false))
                    .count()
            );
        }
    }

    #[test]
    fn content_identity_uses_stable_key_and_hash_not_timestamps() {
        let first = record(Phase::Complete).identity;
        let mut other = first.clone();
        other.modified_ns += 100;
        assert!(first.same_content(&other));
        other.content_hash = "b".repeat(64);
        assert!(!first.same_content(&other));
        other = first.clone();
        other.key = "file:replacement".into();
        assert!(!first.same_content(&other));
    }

    #[test]
    fn model_update_preserves_independent_evidence() {
        let mut r = record(Phase::Complete);
        r.evidence
            .push(evidence(EvidenceKind::TrustedGeneration, true));
        r.model = Some(model(true, true));
        assert!(!r.invalidate_model("approved:test"));
        assert!(r.invalidate_model("new:version"));
        assert_eq!(r.evidence.len(), 1);
        assert_eq!(r.phase, Phase::AwaitingModel);
        assert_eq!(decide(Some(&r), None).bucket, Bucket::Ai);
    }
}

#[cfg(test)]
mod v07_tests {
    use super::*;
    use crate::ai::types::*;
    fn experimental(score: f64) -> MachineRecord {
        let mut record = MachineRecord::unscanned(AssetIdentity {
            key: "v07-fixture".into(),
            content_hash: "a".repeat(64),
            size: 1,
            modified_ns: 0,
        });
        record.component_state = ComponentState::Ready;
        record.release_tier = ReleaseTier::Experimental;
        record.run_state = Some(RunState::CompletedExperimental);
        record.phase = Phase::Complete;
        record.model = Some(ModelResult {
            fingerprint: crate::ai::model::fingerprint(),
            input_scope: "center224".into(),
            raw_score: score,
            threshold: 0.0,
            approved: false,
            high_signal: false,
        });
        record
    }
    #[test]
    fn experimental_completion_and_zero_boundary_are_honest() {
        for score in [-4.0221, -0.000001, 0.0, 0.000001, 100.0] {
            let r = experimental(score);
            let d = decide(Some(&r), None);
            assert_eq!(r.run_state.as_ref().unwrap().label(), "实验检测已完成");
            assert!(!r.model.as_ref().unwrap().approved);
            if score >= 0.0 {
                assert_eq!(d.label, "疑似 AI · 实验");
                assert_eq!(d.bucket, Bucket::Ai);
                assert_eq!(d.badge.as_deref(), Some("AI ? · 实验"));
                assert!(Filter::Suspected.matches(&d, false));
            } else {
                assert_eq!(d.label, "未发现明显 AI 信号 · 实验");
                assert_eq!(d.bucket, Bucket::Unmarked);
                assert!(d.reasons.iter().any(|r| r.contains("不能排除 AI")));
                assert!(Filter::NoSignal.matches(&d, false));
            }
        }
    }
    #[test]
    fn experimental_result_never_overrides_source_or_manual_evidence() {
        let mut r = experimental(-4.0221);
        r.evidence.push(Evidence {
            kind: EvidenceKind::TrustedGeneration,
            provider: "signed fixture".into(),
            summary: "test".into(),
            authenticated: true,
        });
        assert_eq!(decide(Some(&r), None).label, "来源证据 AI");
        let manual = ManualOverride {
            content_hash: r.identity.content_hash.clone(),
            decision: ManualDecision::NotAi,
            created_unix_seconds: 0,
        };
        let d = decide(Some(&r), Some(&manual));
        assert_eq!(d.label, "人工标记非 AI");
        assert!(d.conflict);
        assert!(r.model.is_some());
    }
    #[test]
    fn incomplete_versions_and_wrong_research_boundary_do_not_classify() {
        for corrupt in [
            "fingerprint",
            "threshold",
            "run_state",
            "identity",
            "phase",
            "release_tier",
        ] {
            let mut r = experimental(4.0);
            match corrupt {
                "fingerprint" => r
                    .model
                    .as_mut()
                    .unwrap()
                    .fingerprint
                    .push_str(":other-runtime"),
                "threshold" => r.model.as_mut().unwrap().threshold = -5.0,
                "run_state" => r.run_state = Some(RunState::Expired),
                "phase" => r.phase = Phase::Unsupported("input".into()),
                "release_tier" => r.release_tier = ReleaseTier::Unavailable,
                _ => r.identity.content_hash.clear(),
            }
            assert_ne!(decide(Some(&r), None).bucket, Bucket::Ai, "{corrupt}");
        }
        assert!(!crate::ai::model::stable_approved());
    }
    #[test]
    fn unsupported_is_terminal_and_separate_from_pending() {
        let mut r = experimental(5.0);
        r.model = None;
        r.phase = Phase::Unsupported("transparent".into());
        r.run_state = Some(RunState::Unsupported("transparent".into()));
        let d = decide(Some(&r), None);
        assert!(Filter::Unsupported.matches(&d, true));
        assert!(!Filter::Pending.matches(&d, true));
        let mut counts = FilterCounts::default();
        counts.add(&d);
        assert_eq!(counts.unsupported, 1);
        assert_eq!(counts.pending, 0);
    }
    #[test]
    fn v07_record_round_trips_through_v06_shape_without_new_phase_variants() {
        #[derive(serde::Deserialize)]
        struct LegacyRecord {
            identity: AssetIdentity,
            evidence: Vec<Evidence>,
            phase: Phase,
            model: Option<ModelResult>,
            checked_unix_seconds: u64,
        }
        let r = experimental(-4.0221);
        let value = serde_json::to_value(&r).unwrap();
        let legacy: LegacyRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(legacy.identity, r.identity);
        assert!(legacy.evidence.is_empty());
        assert_eq!(legacy.phase, Phase::Complete);
        assert!(!legacy.model.unwrap().approved);
        assert_eq!(legacy.checked_unix_seconds, 0);
        let mut legacy_value = value;
        for field in [
            "component_state",
            "run_state",
            "release_tier",
            "source_error",
        ] {
            legacy_value.as_object_mut().unwrap().remove(field);
        }
        let loaded: MachineRecord = serde_json::from_value(legacy_value).unwrap();
        assert!(loaded.run_state.is_none());
        assert_eq!(decide(Some(&loaded), None).bucket, Bucket::Pending);
    }
}
