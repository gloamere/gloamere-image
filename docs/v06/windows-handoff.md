# v0.6 Windows development handoff

This is an experimental development candidate, not an install/upgrade artifact or a successful model-quality release. End users continue to receive only a natively validated Setup EXE. Do not install the cloud source ZIP or model component ZIP as the main application.

## Source provenance and safe integration

- Input: `gloamere-image-0.5.0-native-validated-source.zip`, SHA-256 `dc6962042b149f869d30de23e7578af58765726cc1738a95d42156c31ec5306d`.
- Actual Windows development base supplied for handoff: `b84e63d`; main/origin were `cd0a399`. The cloud ZIP is not Git ancestry.
- Do not force-push, reset, overwrite local changes, merge, publish, install, or modify main as part of this source handoff.
- Start on a new local branch from the verified development base. Check actual `git status` and HEAD before any patch application.
- The delivered delta includes per-file preimage and result SHA-256 hashes. Run its check-only command first. An unexpected file, modified preimage, symlink, or missing expected source blocks application. Resolve discrepancies by review, never by changing expected hashes to force acceptance.
- Source archive-only notice files may not be tracked in the native repo. Dependency notices must be regenerated from the final locked Windows dependency tree, not blindly copied from v0.5.

## Native gates

Use official Rust 1.96, Visual Studio C++ Build Tools, Windows SDK and the existing official Inno Setup 6.7.3 workflow. Keep the shared installer identity, preference path and ProgID.

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets --locked -- -D warnings`
3. Build workspace and examples in Debug and Release before running integration tests; use `--test-threads=1` because the existing image pipeline tests share a process-wide memory admission budget.
4. Use `scripts/build-release.ps1` for paired worker builds/tests, PE resources, current dependency notices, payload validation and final Setup EXE. A shortened animation soak is an explicitly labeled smoke only. Run the full 30-minute animation core soak separately before release.
5. Do not call resource-check-only cross compilation a runnable Windows build.
6. Open synthetic fixtures in the actual native UI at 100/125/150/200% DPI and 360×640 / 800×550 / 1180×780. Test keyboard/IME/text focus, cancel/close/pause, rapid navigation and repeated actions.
7. Capture build identity, toolchain/CPU/GPU/RAM, main/worker/Setup SHA-256, test logs and exact model/runtime pack hashes. This document does not authorize installing, changing associations, using clipboard contents or opening private image collections.

## AI acceptance checklist

- 1,000 synthetic directory entries: current image first; queue ≤64 descriptors; cancel/scope change never publishes old results and does not skip retained paths. Close while idle/running/importing, then reopen safely.
- Hide badges: canvas/list overlay disappears, counts/filter/detail/manual state remain. Compare source/clipboard/export hashes or pixels before and after toggling. No watermark, EXIF or sidecar is written.
- Filters are exhaustive: All = AI + Unmarked + Pending + Error; Manual is a cross-cutting view, not a fifth summand. Search intersects filters. No zero-results screen claims all files finished when pending remains.
- Current displayed image remains anchored if it stops matching. Next/Previous acts on the latest filtered set only on explicit navigation.
- Manual AI/Not AI/undo; preserve machine evidence, show conflicts; reopen/change same path, rename/move, same-size/mtime content modification; full SHA-256 gates reuse. Clear auto cache does not clear manual; toggle persistence off, revoke a manual mark, restart and verify it stays revoked.
- Offline import: choose the app-compatible platform pack containing its manifest, pinned model/runtime, notices and provenance. It has no installer, Python, pickle/joblib or custom operators. Bad hash, unknown filename, traversal, symlinked component root and future schema must fail closed. Do not enable a general arbitrary-model picker.
- Component is experimental and manual-only. Automatic source scan does not run it. High raw score is not a percentage and does not put the file in AI. No approved/calibrated threshold exists.
- Current model input admission: opaque 8-bit baseline/progressive JPEG (YCbCr or grayscale) and RGB PNG, no ICC or nontrivial EXIF orientation, short edge ≥224, aspect ratio ≤4:1. CenterCrop224 without resize, ties-to-even offsets; compare supplied golden tensors and logits. JPEG uses the exact pinned libjpeg-turbo 3.1.4.1 component with accurate IDCT and fancy upsampling; 19 RGB fixtures and 12 admitted model tensors match the Pillow reference. RGB-coded/CMYK/arithmetic/lossless JPEG, GIF/WebP/animation/high-bit/transparent inputs are rejected by the experiment, not by the viewer. PNG and JPEG model parity is not a quality benchmark.
- Worker crash, invalid JSON, oversized stdout, wrong identity, timeout and cancellation must recover. Windows JobObject limits process lifetime and a 512 MiB safety ceiling; the ≤256 MiB design goal still requires measured private bytes. Process isolation is not a filesystem/network sandbox.

## Performance and unresolved gates

Pair the exact v0.5 and final v0.6 native builds. Measure first-image/navigation P95, CPU normalization, total/private memory, decoder/model/runtime peaks, cold loading, per-image decode/prepare/inference and cancel latency. The cloud JPEG/PNG parity and core tests cannot establish these native targets. Test setup upgrade/rollback/uninstall without altering the user's real installation. Mac remains unverified.

Quality release is blocked: ≥10,000 independent non-generated images, ≥5,000 isolated-generator generated images, calibrated threshold, confidence bounds, subgroup/unknown-generator/transform coverage, license review and user interpretation tests have not passed. Both tested candidate models miss a known SD3.5 example. This implementation must not be described as completed general automatic AI detection.

## Optional Windows runtime prerequisites

The official ONNX Runtime 1.28.3 CPU DLL imports MSVCP140.dll, MSVCP140_1.dll, VCRUNTIME140.dll, VCRUNTIME140_1.dll and UCRT/system libraries. The main viewer's static CRT flag does not eliminate those component dependencies. Verify clean-machine startup against the supported [Microsoft Visual C++ Redistributable](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist); do not silently install prerequisites. The component files alone are not evidence that runtime startup works on a clean Windows machine.

## Cancellation and filesystem disclosure

Pixel inference is a disposable child process with a 5-second wall timeout and cancellation polling. Ordinary file reads, SQLite storage and component copy/import still use the host filesystem and cannot guarantee interruption of a blocked OS read/write. Import is off the render thread and its owned thread is joined at exit; closing during a stalled filesystem import is therefore an unverified latency case. SQLite's 25 ms/0.5M-operation progress budget does not interrupt an OS filesystem call. No measured<=100ms UI cancel or<=1s shutdown claim is made for all paths. On Unix, runtime loading assumes the component app-data directory is not concurrently changed by another process under the same account; Windows holds the runtime DLL open without write/delete sharing during hash verification and mapping.
