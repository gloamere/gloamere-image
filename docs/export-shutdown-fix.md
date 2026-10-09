# Export ownership during renderer shutdown

2026-10-09. This narrow correction follows a Windows Debug runtime failure: deliberate GPU device destruction during JPEG export was caught at the main renderer boundary, but one zero-byte export temporary file remained. No final JPEG was published and the existing PNG was preserved. The previous candidate's build/resource tests do not cover this failure.

## Cause and design before implementation

`start_export` discards the thread JoinHandle. `ImageApp::drop` only signals cancellation, then the main boundary may call `process::exit(1)`. Rust does not wait for detached threads at process exit. The export thread still owns its `NamedTempFile`; cancellation is not proof that its destructor has run. JPEG flattening occurs after temporary creation and before the first encoder write, explaining the observed zero-byte file.

Keep the file-owning export task's JoinHandle with its existing cancellation token and result receiver. Every export task destruction must cancel and join that exact worker. Normal shutdown and renderer unwinding must explicitly finish this task before returning to the main error boundary. Result polling, disconnect and stale-generation disposal must retain the same ownership rule. Repeated shutdown must be harmless. Read-only loading and in-memory edit/provenance/preview cancellation keep their existing behavior; this change does not add a wait for an unrelated resize.

The worker must never wait for UI acknowledgement: it emits at most one result to its one-slot channel, using nonblocking `try_send`. Export workers do not hold or acquire the UI context. The UI schedules a 50 ms result check only while an export task exists; there is no idle timer. Preserve the existing atomic publication gate: if cancellation wins before publication, no final file appears; if publication already won, retain the complete new file. Never undo a successful publish because a cancellation request arrived later.

Temporary cleanup remains the `NamedTempFile` owner's responsibility. Join proves that its destructor completed before exit; `tempfile` ignores an OS unlink failure, so joining alone cannot promise removal on every filesystem. Do not scan directories, delete by prefix, delete old leftovers, or remove unrelated files. Keep same-directory exclusive creation and no-clobber publication. A hard process kill, abort or power loss is outside orderly shutdown and cannot promise destructor cleanup. Waiting for an in-progress filesystem operation may delay shutdown; do not implement a timeout that abandons the owner and recreates this race.

## Regression plan

- Deterministically pause a worker with an owned zero-byte/partial temporary until cancellation. Destroy its app through renderer-style unwinding without `on_exit`, and assert that the worker and file cleanup finish before the catch boundary returns; preserve a same-prefix unrelated sentinel and source/destination files.
- Cover normal `on_exit` followed by Drop, pending/queued result, worker panic/disconnect and stale generation. No double join, blocked send or unobserved live worker may remain.
- Exercise the real export temporary/encoder path with controlled cancellation and panic; retain existing no-clobber and publication-wins race tests.
- Run formatting, strict Windows-target source check/Clippy and relevant cloud tests. Windows must rebuild and rerun Debug and Release GPU-loss-during-export plus ordinary/cancel export runtime cases. Do not count Linux simulated renderer shutdown as a native GPU pass.

## Implemented limits and evidence

`Work` now owns an optional export JoinHandle. Its Drop cancels and joins, logging a thread panic without panicking during application unwinding. `ImageApp::on_exit` and Drop call the same idempotent shutdown method, which reaps the export before returning. Normal result polling also drops the owner; no completed/stale/disconnected result detaches it. In-memory edit tasks have no file-owning handle.

JPEG flattening checks cancellation before RGB allocation and before every row (at most 32,768 pixels between row checks, with the existing total 16MP input cap). The encoder still checks at every writer call and before final publication; preview pixel arithmetic is unchanged. Codec work between I/O calls, allocation, filesystem write/sync/read/delete and thread scheduling do not have a proven wall-clock upper bound. The 50 ms UI result-check interval is not a shutdown timeout. Under a stalled filesystem, shutdown may wait; there is no timeout that abandons a live file owner. Native local-disk elapsed times must be recorded, and a hung run is a failure rather than a cleanup pass. Forced kill, process abort, power loss and filesystem cleanup errors remain outside the orderly-join guarantee.

The synchronized renderer-unwind regression first failed with cancellation-only disposal (`renderer boundary returned before export cleanup`) and passed after the join implementation. It holds an owned zero-byte or partial temp behind a release channel, proves that shutdown waits while the owner is live, then verifies cleanup before return. It also preserves the original, a completed PNG and an unrelated same-prefix temporary file.

Additional tests cover normal shutdown after successful publication without UI result consumption, while the UI context lock is held; repeated shutdown; normal and stale queued cancellation-result ownership; thread panic/disconnect; the real export encoder/temp path with zero-byte or partial cancellation; and row-level cancellation with identical matte arithmetic. Existing publication-wins, cancellation-wins, no-clobber, disk-full and encoder-panic regressions remain in place.

Cloud Rust tests: 101 passed, 0 failed, 5 Windows-worker tests ignored (UI 57, image integration 17, library 21, provenance/profile 4, fault-helper path 2). Formatting and strict Windows-target check/Clippy passed using the explicit source-check-only resource mode, not native Windows linking. Python regression suites passed 85 tests (packaging 45, migration 21, default applications 14, PE parser 5). No application layout, icon assets, installer logic, dependency, product identity or default-app policy changes in this correction.

## Windows retest handoff

1. Import the new byte-identified source candidate into a new QA directory and verify its external SHA-256 and SOURCE-MANIFEST. Rebuild Debug and Release from this source and rerun all tests, including actual worker tests. Do not reuse r3 binaries.
2. Rerun ordinary PNG/JPEG export, explicit cancel and window-close-during-export for both profiles. Record exit duration, input hashes, existing output hashes, final file status and every fixture-directory temp before/after. A same-prefix foreign sentinel must remain untouched.
3. Repeatedly rerun the deliberate logical-device destroy during JPEG export in Debug and Release. Confirm the ready report records an in-flight JPEG; collect the caught-failure report, exit code, elapsed time, and absence of newly orphaned owned temps after the process exits. A previously completed PNG remains. If publication won first, a complete valid JPEG is allowed; partial or zero-byte final files are not.
4. Rerun normal D3D12 reopen afterward. The 16MP navigation/edit/PNG/JPEG workflow remains within existing memory limits; no memory improvement is claimed. Recheck active-export progress and cancellation response with the UI result polling change; confirm idle repaint remains idle.
5. Run the unchanged official release/Setup pipeline with all gates, then relevant isolated installer smoke checks against the rebuilt binaries. Preserve existing real v0.3 installation unless that separate target is explicitly authorized.

The Windows r3 Debug export-cleanup failure remains valid even though one Release run passed. Different scheduling can hide a detached-thread race. Previously reported r3 Debug/Release test and Setup-resource successes remain historical evidence only. Native GPU/runtime outcomes for this new source are pending.

## Official lifecycle references

- https://doc.rust-lang.org/std/thread/struct.JoinHandle.html#method.join
- https://doc.rust-lang.org/std/process/fn.exit.html
