# WIP source migration snapshot

**Source backup only. Online updating and the latest native fixture edits remain UNACCEPTED and incomplete.**
No application build, product security validation, actual installation or release qualification was performed for this migration.

Remote parent: `cd0a399bc46d5fda82966f7e6e5f1fd42351a2ca`.
Original independent local HEAD: `52112232c96fbcbb0469541114ee2acf6dc48d21`.
Original local baseline/main: `79e930cb4103a60c5c032713275b3f7aeff806cf`.
Reviewed input manifest SHA256: `838b2530d7e1aba154aa052326d340dc73b7044cd852f04d5b0be277971a6950`.
Offline source ZIP SHA256: `afb7d5bac40a87ae71b303be61fe87e43cc0be365fda43f534d49d304627843f`.
Offline complete reachable-history bundle SHA256: `c902fad1afd6f6477d092b8eaf3a50feff3dd770dbeaedb436a45e299971129d`.

The original local dev history and dirty working tree are preserved separately. They are not represented as ancestors of this commit.
All 83 remote-only documentation/license paths are retained byte-for-byte. The 35 reviewed common-path changes use the latest local WIP semantics.
Five uncommitted source edits and two untracked design documents are included. No credentials, generated binaries or model weights are intentionally imported.

UI differences remain explicit: failed cross-directory opens retain the attempted selection instead of restoring the old selection; the list-row coexistence test changed from 800x550 to 1180x780 with separate narrow-drawer coverage. Two provenance integration tests were renamed for cross-platform worker selection. No regression-free claim is made.

## Raw test outputs retained offline

The following historical stdout logs, referenced by the historical update-security VERIFICATION.md, are retained in the original repository, ZIP and Git bundle rather than this source tree. Those old verification statements are not validation of this WIP commit.

- `crates/update-security/docs/bootstrap-clippy-2026-10-10.txt`
- `crates/update-security/docs/bootstrap-fixture-clippy-2026-10-10.txt`
- `crates/update-security/docs/bootstrap-fixture-tests-2026-10-10.txt`
- `crates/update-security/docs/bootstrap-tests-2026-10-10.txt`
- `crates/update-security/docs/clippy-results-2026-10-10.txt`
- `crates/update-security/docs/fixture-clippy-2026-10-10.txt`
- `crates/update-security/docs/fixture-tests-2026-10-10.txt`
- `crates/update-security/docs/test-results-2026-10-10.txt`

## Reviewed shared paths

- `Cargo.lock`
- `Cargo.toml`
- `README.md`
- `build-support/test_windows_resources.py`
- `build-support/verify_windows_resources.py`
- `build-support/windows_resources.rs`
- `build.rs`
- `crates/provenance-worker/Cargo.toml`
- `crates/provenance-worker/src/main.rs`
- `installer/default-apps-types.iss`
- `installer/default-apps.iss`
- `installer/light-image-tool.iss`
- `installer/menus.iss`
- `installer/recovery.iss`
- `scripts/build-installer.py`
- `scripts/build-release.ps1`
- `scripts/collect-licenses.py`
- `scripts/package-source.py`
- `scripts/stage-installer-payload.py`
- `scripts/test-branding-migration.py`
- `scripts/test-default-apps-installer.py`
- `scripts/test-release-packaging.py`
- `src/app.rs`
- `src/app_keyboard_tests.rs`
- `src/browsing.rs`
- `src/export.rs`
- `src/lib.rs`
- `src/main.rs`
- `src/pipeline.rs`
- `src/settings.rs`
- `src/viewer_ui.rs`
- `src/worker.rs`
- `tests/provenance.rs`
- `tests/worker_faults.rs`
- `third-party-notices/inventory.json`

## Preserved remote-only paths

- `docs/architecture-implemented-v0.2.md`
- `docs/branding-design-v0.4.md`
- `docs/branding-icons.md`
- `docs/branding-installer-migration.md`
- `docs/cancellation-publication-order-design.md`
- `docs/cjk-fallback-validation-design.md`
- `docs/compact-actions-design-v0.4.md`
- `docs/compiler-version-gate-fix.md`
- `docs/crop-confirmation-layout-design.md`
- `docs/default-apps-v0.4.md`
- `docs/dependencies.md`
- `docs/export-shutdown-fix.md`
- `docs/fault-injection-design.md`
- `docs/gpu-fault-design.md`
- `docs/image-list-readability-design.md`
- `docs/interaction-acceptance-design.md`
- `docs/interaction-export-provenance-v0.2.md`
- `docs/lightweight-optimization-design.md`
- `docs/next-stage-design.md`
- `docs/offline-test-fixtures.md`
- `docs/offline-worker-design.md`
- `docs/owned-budget-and-runtime-design.md`
- `docs/rapid-browsing-cancellation-design.md`
- `docs/release-path-mapping-design.md`
- `docs/releases/v0.4.0.md`
- `docs/reproducible-release-design.md`
- `docs/review-hardening-design.md`
- `docs/toolbar-visibility-design.md`
- `docs/ui-design-v0.4.md`
- `docs/ui-readability-and-evidence-design.md`
- `docs/upgrade-recovery-design.md`
- `docs/visual-crop-design.md`
- `docs/windows-path-browsing-design.md`
- `docs/windows-qa-v0.4.md`
- `third-party-notices/accesskit-0.24.1/00-LICENSE-APACHE`
- `third-party-notices/accesskit-0.24.1/01-LICENSE-MIT`
- `third-party-notices/accesskit-0.24.1/02-LICENSE.chromium`
- `third-party-notices/accesskit_consumer-0.35.0/00-LICENSE-APACHE`
- `third-party-notices/accesskit_consumer-0.35.0/01-LICENSE-MIT`
- `third-party-notices/accesskit_consumer-0.35.0/02-LICENSE.chromium`
- `third-party-notices/accesskit_windows-0.32.1/00-LICENSE-APACHE`
- `third-party-notices/accesskit_windows-0.32.1/01-LICENSE-MIT`
- `third-party-notices/accesskit_windows-0.32.1/02-LICENSE.chromium`
- `third-party-notices/accesskit_winit-0.32.2/00-LICENSE-APACHE`
- `third-party-notices/accesskit_winit-0.32.2/01-LICENSE-MIT`
- `third-party-notices/accesskit_winit-0.32.2/02-LICENSE.chromium`
- `third-party-notices/alloc-stdlib-0.2.4/00-LICENSE`
- `third-party-notices/asn1-rs-impl-0.2.0/00-LICENSE-APACHE`
- `third-party-notices/asn1-rs-impl-0.2.0/01-LICENSE-MIT`
- `third-party-notices/btree-range-map-0.7.2/00-publication-license-declaration.txt`
- `third-party-notices/c2pa-0.91.2/00-LICENSE-APACHE`
- `third-party-notices/c2pa-0.91.2/01-LICENSE-MIT`
- `third-party-notices/clipboard-win-5.4.1/00-LICENSE`
- `third-party-notices/delegate-0.8.0/00-LICENSE-APACHE`
- `third-party-notices/delegate-0.8.0/01-LICENSE-MIT`
- `third-party-notices/ecolor-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/ecolor-0.36.2/01-LICENSE-MIT`
- `third-party-notices/eframe-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/eframe-0.36.2/01-LICENSE-MIT`
- `third-party-notices/egui-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/egui-0.36.2/01-LICENSE-MIT`
- `third-party-notices/egui-wgpu-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/egui-wgpu-0.36.2/01-LICENSE-MIT`
- `third-party-notices/egui-winit-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/egui-winit-0.36.2/01-LICENSE-MIT`
- `third-party-notices/emath-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/emath-0.36.2/01-LICENSE-MIT`
- `third-party-notices/epaint-0.36.2/00-LICENSE-APACHE`
- `third-party-notices/epaint-0.36.2/01-LICENSE-MIT`
- `third-party-notices/iref-core-3.2.2/00-LICENSE-APACHE`
- `third-party-notices/iref-core-3.2.2/01-LICENSE-MIT`
- `third-party-notices/jfifdump-0.6.0/00-LICENSE-APACHE`
- `third-party-notices/jfifdump-0.6.0/01-LICENSE-MIT`
- `third-party-notices/profiling-1.0.18/00-LICENSE-APACHE`
- `third-party-notices/profiling-1.0.18/01-LICENSE-MIT`
- `third-party-notices/range-traits-0.3.2/00-publication-license-declaration.txt`
- `third-party-notices/rasn-cms-0.28.15/00-LICENCE-APACHE`
- `third-party-notices/rasn-cms-0.28.15/01-LICENCE-MIT`
- `third-party-notices/rasn-ocsp-0.28.15/00-LICENCE-APACHE`
- `third-party-notices/rasn-ocsp-0.28.15/01-LICENCE-MIT`
- `third-party-notices/rasn-pkix-0.28.15/00-LICENCE-APACHE`
- `third-party-notices/rasn-pkix-0.28.15/01-LICENCE-MIT`
- `third-party-notices/static-regular-grammar-2.0.2/00-publication-license-declaration.txt`

Production trust remains unconfigured. Ordinary runtime updates do not constitute a complete authenticated launcher/protocol self-upgrade path. No main merge, history rewrite, force push, deployment, certificate or permission change is part of this migration.
