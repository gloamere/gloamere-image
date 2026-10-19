# v0.6 folder screening UI

This is a source implementation, not Windows/macOS native acceptance or a validated general AI detector. Existing v0.5 image/edit/export/clipboard/animation code paths remain responsible for document pixels. The AI UI only submits commands and paints decoration.

## Entry points and semantics

- Keep the v0.5 7/8/11 responsive icon toolbar unchanged. Open the image list from More. Its vector funnel opens All, AI, Unmarked AI, Pending, Error and Manual views. AI includes suspected results by default.
- Filename search intersects the selected category. Counts describe the entire current folder, not the filename query. The four primary categories partition All; Manual overlaps those categories. When suspected results are hidden, AI shows both visible and full counts.
- List ordering stays the existing natural folder order. Asynchronous results rebuild row membership, never the loaded image, current path, focus, or scroll request. If a current image leaves the filter, the canvas remains anchored with an explanatory note. The next explicit Previous/Next/Home/End action uses the latest filtered-and-searched set; an empty set does not change the image.
- Stable row interaction IDs use paths rather than filtered row numbers. A filename field with focus owns arrow/Home/End keys. Scan completion does not request keyboard focus.
- At widths below 640 logical points the list collapses without changing the saved list preference. AI choices remain available in More. Below 800 points the existing inspector uses a bottom drawer, preserving the compact toolbar.

## Evidence, correction and overlays

The existing Info inspector now begins with AI screening details, separate from the existing C2PA/source inspector. It shows effective labels, reasons, task phase, inspection time, evidence provider/summary/authentication, retained model technical records when present, and active/stale manual decisions. Scores are explicitly described as scores, not probabilities. Experimental output says no release threshold is configured; upstream diagnostic boundaries are not shown as calibrated product thresholds. The current experimental pixel scope is opaque 8-bit baseline/progressive JPEG (YCbCr or grayscale) and RGB PNG, with no ICC or transformed EXIF orientation, short edge ≥224 and aspect ratio ≤4:1. JPEG uses the pinned libjpeg-turbo 3.1.4.1 decoder. WebP/GIF, animation, high-bit-depth, transparent and unvalidated JPEG modes remain unsupported for pixel inference; existing viewing and source metadata checking remain supported.

The default is local source metadata checking only. Missing source metadata and no approved pixel model remain pending/awaiting-model, never “real” or a completed non-AI conclusion. This UI explicitly names the missing/unapproved model limitation, the current-folder/nonrecursive scope, and the absence of automatic uploads/downloads. Ordinary metadata is not authenticated provenance. Current animation/static-frame and edited-copy limitations are shown.

“Mark AI,” “Mark non-AI,” and “Reset manual mark” use verified content identity through the controller. Until a fingerprint exists, marking is disabled; manual current-file checking remains available. Correcting the effective category does not delete machine evidence. A changed original can leave a stale manual record, shown as requiring confirmation.

Canvas and thumbnail badges use only the egui painter. They never modify an image buffer, original metadata, clipboard buffer or export. Turning badges off leaves scan settings, cached results, filter sets, counts and details untouched. Clicking a canvas badge opens the same Info inspector. For a locally edited canvas the badge explicitly refers to the disk original.

## Settings and clearing scope

- Automatic source checking, badge display and machine-result persistence are independent controls.
- An explicit offline folder picker queues component verification/import in the background. Status says pending until the controller reports completion. Imported experimental model scores are detail-only and do not generate automatic AI labels. Removing the model has its own confirmation and preserves manual decisions/source evidence.
- Pause/resume appears in the list. Retry errors, refresh folder and manual current-file checking remain available in details/settings. An active worker schedules short repaint polling; terminal awaiting-model states do not keep polling indefinitely.
- v0.5 settings.json is unchanged. The controller stores AI results in ai-records.sqlite and AI preferences in ai-settings.json, both alongside the existing settings file. Test/smoke constructors use an in-memory controller.
- Clear automatic records and clear manual records have distinct confirmation text. Both apply to this user's local store across all previously visited folders, not just the visible list. Automatic clearing preserves manual decisions and source images; manual clearing preserves automatic evidence and images and is labeled irreversible.
- A true egui modal blocks the underlying viewer during a clear confirmation. Escape, the close icon and Cancel dismiss without clearing. Animation is suspended while this confirmation is open. No clear operation is triggered by simply opening settings.
- Clipboard/extracted-frame sessions clear the folder scope and make their absent file provenance explicit. Exiting shuts down the controller with other owned background workers.

## Verification

`src/app_ai.rs` includes headless real-egui regressions for anchored filtering/search navigation, badge/scan/filter/pixel independence, pointer-driven manual AI/non-AI/reset, filename focus across result updates, clear-dialog keyboard/cancel interruption, and narrow-window list/toolbar behavior. Run them and the full existing viewer tests with the repository toolchain and a single test thread because the shared memory-budget tests deliberately use global state:

    cargo test --bin gloamere-image -- --test-threads=1

Native Windows mouse/IME, accessibility, actual GPU rendering, 100/125/150/200% DPI, install/upgrade and long-running resource/performance acceptance remain release gates. Headless tests do not replace those checks. Test outcomes and final counts belong to the release verification report, not this implementation description.

Opt-in actual-egui CPU captures (no GPU dependencies) are available through the ignored render test:

    GLOAMERE_AI_RENDER_DIR=/path/to/qa/ui cargo test --bin gloamere-image render_actual_ai_ui_states -- --ignored --test-threads=1

The output includes source-only, manual AI + filter, open filter popup, narrow drawer and narrow clear-confirmation PNGs. The synthetic image and unsigned test metadata are fixtures, not detector quality samples. Renderer output is based on the application's real tessellated shapes and uploaded texture deltas; CPU sampling is not a claim of exact native GPU text/edge equivalence.
