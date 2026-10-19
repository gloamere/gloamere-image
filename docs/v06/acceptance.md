# v0.6 experimental candidate: cloud verification

Date: 2026-10-09. This is a development handoff, not a signed Windows release, installer or completed general automatic AI detector.

## What works in this candidate

- Local, bounded source-metadata screening; current image → visible list rows → remaining folder scheduling, pause/cancel/retry and stale-result rejection.
- Exhaustive, stable folder filters; anchored current selection; independent nondestructive badges and scan settings; manual AI/non-AI/reset decisions that preserve machine evidence.
- Versioned SQLite automatic-result cache and independent durable manual records, full-content SHA-256 revalidation, LRU, persistence and explicit clearing controls.
- Optional, offline-imported CoDE FP32 + linear-head ONNX component with an actual killable local worker. Manual JPEG/PNG inference produces raw technical scores. No Python runtime or heuristic pixel detector is shipped.
- Fixed model, runtime and JPEG decoder inventories/hashes. A manifest, edited cache or raw score cannot grant quality approval. Experimental scores do not change automatic badges or AI filter membership.
- Shared Rust/egui implementation. Existing v0.5 pixels, rotation, editing, export, clipboard and animation paths are preserved; badge painting is separate.

## Reproducible cloud checks

The cloud machine is Linux x86_64. Rust 1.96.0 is scoped to the test workspace. The original product remains Windows/macOS-oriented; the supplied cloud harness changes only Linux backend/dependency wiring and references the actual source files. It is not a Windows build.

| Check | Observed result | Evidence in handoff |
|---|---|---|
| Untouched v0.5 baseline, serial | 177 passed: 60 library, 81 viewer, 19 animation, 17 core; long soak not run | qa/logs/baseline-serial.log |
| Final Debug regression suites | 243 passed: 115 library, 90 viewer, 19 animation, 17 core, 2 fault-support; 3 opt-in/platform tests not run by this command | qa/logs/final-debug-tests.log |
| Final Release regression suites | 243 passed; same 115/90/19/17/2 selected suites, 3 opt-in/platform tests skipped | qa/logs/final-release-tests.log |
| Actual C2PA worker | 4 SDK worker unit tests and 8 process integration tests passed, including signed/tampered/remote/OCSP fixtures | qa/logs/provenance-unit.log, provenance-full.log |
| Installer/packaging/resource Python checks | 101 passed: 52 packaging, 17 installer, 27 migration, 5 resource | qa/logs/python-*.log |
| Formatting / clippy | Final command outcomes recorded with source identity | qa/logs/fmt.log, clippy-final.log |
| Windows-specific code | Isolated stable FileId/JobObject/network-drive adapter and JPEG adapter cross-checks passed for x86_64-pc-windows-msvc | qa/logs/windows-adapter-check.log, jpeg-windows-check.log |
| Actual egui layout renders | Five real tessellated egui CPU captures, including 360×640 drawer and confirmation | qa/ui/ |
| Independent focused review | See final focused report; policy combinations, queue generations, cache locking, bounds and codec fixtures checked | qa/review/ |

The baseline's default parallel test invocation exposed the existing shared global-memory-budget interference. The documented serial rerun passes; serial results are not a production responsiveness measurement. The 30-minute animation soak, actual native Windows fault/JobObject suite and native GPU/clipboard/installer tests remain separate gates. The opt-in UI render test is run separately.

## Exact decoder, tensor and model evidence

The input contract is `code-rgb-center224-imagenet-ljt3141-v2`: opaque 8-bit RGB PNG or baseline/progressive Huffman YCbCr/grayscale JPEG; no ICC transform or nontrivial EXIF orientation, no resize, short edge ≥224 and aspect ratio ≤4:1. Center crop uses Python ties-to-even offsets, RGB, ImageNet normalization and planar float32. JPEG supports the verified 4:4:4/4:2:2/4:2:0 and grayscale paths only. Animation, WebP/GIF, transparent/high-bit-depth inputs and other JPEG modes are explicitly unsupported for this experiment, while ordinary viewing remains available.

- Pinned libjpeg-turbo 3.1.4.1: 19/19 JPEG decoder fixtures, 4,323,402 RGB channels, zero byte differences against Pillow 12.3.0/libjpeg-turbo 3.1.4.1. The source example also checks malformed and unsupported cases.
- Actual Release application worker: all 4 original admitted PNG/JPEG vectors and all 12 expanded JPEG vectors have bit-exact input tensors. Maximum absolute raw-logit error is 0.0000357573 against the independent PyTorch encoder plus original float64 numerical linear head (tolerance 0.0001).
- Whole-job times on this small fixed cloud corpus were 192.2–253.4 ms. These include process start, component verification, decoding and inference; they are not a representative P95 or Windows performance result.
- Cancellation checks reaped the worker in 21.7–21.8 ms; 1 ms deadline tests returned and reaped in 11.7–12.4 ms, then a new inference succeeded. These are measured test cases, not universal OS I/O guarantees.
- Separate native ORT 1.28.3 CPU probe: 100 runs per each of three stored tensors; two-thread worst warm P95 about 32.0 ms and peak total RSS about 58.8 MiB. This measures that probe, not application private bytes, JPEG worst-case memory or native Windows performance.

The initial default Rust JPEG decoder differed from the reference (one diagnostic logit delta 0.0342013). It was not accepted by loosening tolerance. The optional pinned matching decoder fixes that tested path. The main viewer decoder was not replaced.

### Pinned components

| File | Bytes | SHA-256 |
|---|---:|---|
| code-linear-fp32.onnx | 22278043 | 5a888282941391e2249af4da8af54ee36a822ff3d09c22f2e6cb4a6be6dc2e7b |
| Windows onnxruntime.dll | 15828832 | 4d2774a5f64e4a230b16b74b167171e92f65b0683c19680c1d04fb284b92a6e2 |
| Windows onnxruntime_providers_shared.dll | 21856 | 6582418469facb25b33bc212d8efdc8f7ee57d0c8161e57a59434377780a38b4 |
| Windows turbojpeg.dll | 1133568 | 4a171bfe14f3032fcaad3db6c9a199b0d81cf6ca8ce3b6f4c98809df76fadeb8 |

Model input is `pixel_values` float32 [1,3,224,224], output `ai_logit` float32 [1,1], ONNX opset 14. The complete graph is pinned and loaded from the exact rehashed buffer, with no custom operators or external weights. ORT 1.28.3 uses CPU, two intra-op/one inter-op threads, no spinning and telemetry disabled. Required Microsoft Visual C++ runtime availability must be tested on native Windows; no prerequisite is silently installed.

## Quality and product-release gates still open

1. General automatic pixel classification is blocked. Neither CoDE nor the separately tested Community Forensics candidate passed the approved dataset/calibration gate; both miss the same explicitly SD3.5-generated smoke example. No ≥10,000 independent non-generated / ≥5,000 generated quality benchmark, calibrated threshold, confidence interval, unknown-generator or transformation acceptance has passed. Technical parity cannot substitute for those tests.
2. Automatic checking currently uses bounded unsigned source metadata. The existing C2PA inspector remains separate; there is no new trusted-issuer snapshot or cross-inspector promotion into trusted AI badges. Unknown/missing evidence is never “real.” Compatible watermark metadata is not cryptographic provenance.
3. Native Windows full-crate build, actual DLL decode/model parity, clean-machine startup, accessibility/IME/DPI/GPU/clipboard, 1,000 mixed-file UI directory, installer upgrade/rollback/uninstall and EXE signing remain unverified here. Mac has no approved component/runtime or native acceptance.
4. The design's paired native first-image/navigation latency, CPU ≤25% normal-mode average, ≤256 MiB additional private bytes and large-image peaks remain unmeasured. One worker/two threads, bounded input/queue, explicit pause and process exit are implemented. Low-battery and sustained-interaction adaptive throttling are not yet implemented. The Windows 512 MiB JobObject safety ceiling is not proof of the 256 MiB target.
5. Ordinary filesystem I/O and startup SQLite migration cannot be forcibly interrupted. SQL busy wait is zero and VM work is limited; renderer polling consumes at most four responses and checks an 8 ms elapsed budget between results. This does not cap a single OS call. Import runs off the UI thread but joins on shutdown, so a blocked filesystem import can delay exit. The runtime is process-isolated, not an OS filesystem/network sandbox.
6. Existing automated pixel/export and animation regressions pass, and UI tests establish badge/state independence. They do not prove every real application clipboard/format interoperability case or the full 30-minute soak.

## Resources, licenses and delivery

The source handoff includes regenerated locked Windows dependency notices and the optional pack includes model, ONNX Runtime and JPEG licenses/provenance. The baseline notice process's three registry-declaration fallbacks remain identified in the inventory; automated notice collection is not a legal opinion. Component hashes and compressed/uncompressed archive sizes are recorded in the delivery manifest. No claim is made that the final main Setup EXE remains about 10 MB.

This software is based in part on the work of the Independent JPEG Group.

Follow `windows-handoff.md` and the check-only delta workflow. The cloud source ZIP has no Git ancestry; no branch was pushed and no installation, association, real clipboard or private image collection was touched.
