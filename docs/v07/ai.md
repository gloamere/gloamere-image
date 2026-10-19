# v0.7 AI implementation addendum

Approved source: `gloamere-image_v0.7_AI检测与静默更新设计_2026-10-10.docx`, sections 2–5 and 14. Implementation begins 2026-10-10.

## Invariants fixed before implementation

- Component readiness, per-image execution, and release tier are independent. Successful experimental inference is complete, never unsupported merely because the model is experimental.
- The existing research boundary remains exactly zero. Positive output is at most “疑似 AI · 实验”; negative output says “未发现明显 AI 信号 · 实验” and “不能排除 AI”. No image-specific rule or threshold adjustment.
- Source-only, manual, experimental-folder, and stable-auto requests have explicit causes. Experimental folder scanning defaults off and requires a separate risk acknowledgement. No stable detector is approved in this build.
- Automatic scope is the current local folder, nonrecursive, with current/visible items prioritized, one worker, cancellation and generation/ticket checks. Network folders are not scanned automatically.
- Exact model/runtime/decoder hashes remain pinned. Readiness requires full manifest verification and a synthetic local worker self-test. No model download is offered without a real trusted distribution source.
- `ai-settings.json` remains schema 1. New opt-in settings use `ai-experimental-settings.json`; cache database schema stays 2 and manual records remain independent. New state fields are additive JSON fields while legacy `phase` remains parseable by v0.6. Old results with incomplete fingerprints expire rather than silently becoming new experimental decisions.
- Cache identity covers content SHA-256, model/runtime/preprocess/protocol/research policy; approval never comes from cache, imported manifests, or UI settings.
- UI overlays, filters and local records never modify original pixels, metadata, copied/exported content, or provenance.

## Validation to record

Unit/controller regressions; full existing tests; CPU egui render regressions at desktop and narrow sizes; real isolated Linux worker result on the known missed fixture. CPU meshes are not native Windows/GPU acceptance, and state fixes are not model-quality validation.

## Implemented architecture

`ComponentState` is missing/checking/ready/failed. Readiness is established asynchronously by checking the exact pinned complete package and executing a deterministic 224×224 synthetic image through the same isolated worker. The baseline reference is 0.25280970335006714 with ±0.01 operational tolerance. This self-test only checks execution integrity; it is not detector calibration or accuracy evidence.

`RunState` stores source-only/source failure, disabled, queued, running, paused, experimental completion, unsupported, expired and failure/timeout. Source-only queue/running labels explicitly identify source checks. `ReleaseTier` is displayed separately. No shipped model has stable approval. `QueueCause` replaces the old manual boolean with user-manual, experimental-folder, stable-automatic and source-only causes. Stable automatic remains hard-gated off.

The experimental preference is opt-in and requires a separate risk dialog. Import does not turn it on. It lives in `ai-experimental-settings.json` (schema 1, acknowledgement revision 1); unknown/future or corrupt preferences are preserved and treated as off. Existing `ai-settings.json` remains byte-for-byte unchanged when toggling this preference.

Records have additive v0.7 JSON fields while legacy `phase` stays in the v0.6 enum. The SQLite schema stays 2, manual records are untouched, and all cached approval is still stripped. A full fingerprint now binds model hash, preprocessing version, exact platform runtime hash, JPEG decoder hash, worker protocol and the zero-boundary research policy. Legacy/incompatible results expire and their existing persistent model payload is retained until re-detection. Reusing a valid cached pixel result retains its original pixel-result timestamp, independently of a new source check.

Scope filters reject recursive/mixed-folder lists and symlink entries. UNC/mapped network locations never create automatic pixel jobs; an explicitly opened current network file may still receive the existing source-only check. The worker re-hashes after inference before publishing, and generation/ticket/cancellation guards reject superseded responses. Clearing automatic records cancels pending work and does not let polling immediately refill the cleared cache. The UI separates actual pixel-completion counts from source checks.

The existing `Controller::import_model_pack(&Path)` now shares the owned activation lock and publishes a legacy-component pointer only after whitelist verification and the fixed self-test. Existing valid components are never overwritten by a failed import. Signed model updates use the separate opaque-target staging API described below; the bundled production repository remains unconfigured and therefore no real network update is enabled.

## Validation status (2026-10-10)

- All 66 AI library tests pass, including v0.7 policy, consent/settings rollback, unsupported-terminal filtering, scope, stale cache, cancellation and non-reappearance coverage.
- Existing AI UI interaction regressions plus risk-dialog cancellation pass serial (10 tests). Parallel execution can exhaust the pre-existing process-global allocation budget during fixture setup; use `--test-threads=1`.
- Independent real-worker Controller probe reproduced the known miss: −4.022103309631348, completed experimental, approved=false, high_signal=false, negative label plus “不能排除 AI”. It also verified fixed self-test, opt-in, unchanged original bytes, manual/badge independence, content replacement, folder bounds and restart behavior.
- Fourteen AI UI tests, including all three CPU render cases, passed. After the final settings-navigation adjustment, all 12 affected interaction/full-app-render cases passed again; the two unchanged shared-state/record render cases remain covered by the preceding full run.
- 32 inspected CPU PNGs are in `qa/ai-cpu-ui`: seven full ImageApp views, twenty synthetic state-panel views, four panels bound to genuine positive/negative worker records and a measured 50px thumbnail badge. The settings button now scrolls directly to component controls; full experimental wording remains in details and canvas, with `AI? 实` only in the 50px thumbnail.
- Genuine positive fixture: CC0 Stable Diffusion cyberpunk image, losslessly converted from WebP to a supported PNG for this explicit test, raw score +10.610712051391602. The negative user fixture remains −4.022103309631348. Both complete experimentally, retain approved=false/high_signal=false and use the unchanged zero boundary. Neither result establishes model accuracy.
- The final library run still passes 66 AI tests after model removal was changed to wait for the canceled worker before deleting runtime files. This prevents a delete-versus-open-DLL race; native Windows execution remains untested.
- CPU meshes, Linux model execution and reference fixtures do not substitute for native Windows GPU, installer or performance acceptance.

## Signed model-update addendum (approved continuation)

The model-update preference must have a complete execution loop when a trusted repository is configured. App updates and installed-model updates have independent consent; model updating never turns on experimental folder screening or changes quality tier. Production trust/IP-HTTPS configuration remains absent.

Implementation contract: independent signed model target/sequence/quarantine; exact compiled model/runtime/preprocess/protocol/license/manifest pins; bounded flat ZIP extraction with no unknown files, paths or links; fixed worker self-test in staging; immutable version directories and atomic active pointer retaining the previous pack. Automatic updates require the same already-installed model ID. A new graph or runtime requires a reviewed application allowlist change, even when its remote target is signed. Preference and cancellation are rechecked immediately before activation. Startup validates active/previous candidates and may restore only the retained previous verified component. Pack revision participates in record cache binding; manual marks and experimental-screening consent remain independent.

### Implemented signed-component loop

`ModelJob::start` checks the independent signed model target, checks the installed model and both download/model-volume budgets, downloads a content-addressed ZIP, and delegates to the same `stage_verified_archive` implementation available for an already verified artifact. That API requires an opaque `VerifiedModelTarget`; a caller-written JSON file or a download path alone has no authority. Debug-only repository fixtures exercise the production verifier without provisioning a server or persistent signing key.

Only the existing CoDE experimental model ID and compiled profile can be updated. The manifest and full flat file inventory, including license/provenance text, have compiled hashes. ZIP extraction rejects extras, path traversal, links, duplicate names, encryption, unexpected compression, ZIP64/multivolume/SFX/comments, oversized central directories and aggregate/per-entry expansion overflows. Hashing uses the same opened archive before and after extraction. A staged fixed synthetic worker self-test must pass before a durable immutable version directory is created.

Activation is a separate UI-thread boundary: cancel/join the current pixel worker, revalidate the opaque target against locked current TUF state, compare the live installed pointer, then re-read independent persisted model-update consent under the publication gate. Windows namespace ancestors are pinned, the version rename and pointer replacement are write-through, and the old directory is retained. Manual/resource pause preserves verified partial-download bindings; explicit cancellation clears only the exact partial pair after the download lock is released. Turning either update preference on never authorizes experimental scanning or a stable quality tier.

Startup verifies and self-tests the active component. If it fails, only the exact retained previous component may be validated and restored. A failed signed sequence is persisted in the model quarantine and bridged into the independent trust floor before another selection/activation. Explicit uninstall writes a tombstone first, so legacy or retained bytes cannot resurrect on restart.

The additive `pack_revision` cache field distinguishes signed package sequences independently from the unchanged research fingerprint. A revision change expires automatic pixel results while preserving original cached payloads, source evidence, manual records, and all screening preferences. v0.6 JSON/schema rollback compatibility is retained.

### Additional verification

The first integrated model-update run passed all 198 library tests. Eight new archive/publication tests cover malicious formats, exact hashes, consent/cancellation at the pointer boundary, tombstones and non-overwriting promotion. The AI revision regression verifies that an identical model fingerprint with a different package revision expires without erasing cached/manual records. Pause-specific and exact-partial-cleanup regressions were added next for the final integration run. Real signed-pack activation/restart/recovery is checked independently using an ephemeral test repository and the genuine pinned Linux component; native Windows runtime acceptance remains a separate release gate.

The independent signed real-pack probe passed all six lifecycle checkpoints; its concise evidence log is `qa/model-signed-activation-probe.log`. It verified live preference off, persisted preference off, cancellation immediately before publication, genuine signed ZIP plus actual worker self-test and Controller activation, restart of the activated component, and intentional corruption of a temporary installed graph followed by exact old-pack recovery and durable signed-sequence quarantine. Source bytes, manual marks, experimental scanning off, and application-update consent off were preserved. These are local Linux execution and authenticated in-memory metadata tests, not a production server or native Windows acceptance claim.

Final integration after pause/cancel, model-volume and quarantine changes: 202 library tests passed; 92 UI tests passed (3 intentionally ignored in that invocation); all-target strict Clippy passed. The earlier explicitly run CPU render cases and the independent genuine signed-model lifecycle probe remain separate evidence. Final owned-source digests are recorded in `qa/ai-model-source.sha256`.
