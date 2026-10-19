# Verification record — 2026-10-10

## Executed

- Rust 1.96.0, Linux x86_64.
- `cargo test -p safeupdate-core`: 77 tests passed, 0 failed; 0 doctests. This final run covered 31 transport/downloader tests and 46 authenticated-metadata/state/model tests, with `CARGO_INCREMENTAL=0` and two build jobs. Full log: `test-results-2026-10-10.txt`.
- `cargo test -p safeupdate-core --features fixture-harness`: 79 tests passed, 0 failed; 0 doctests. The two additional cases verify genuine signed fixture-model authority and reject corrupt signatures, unsupported filenames and oversized metadata. Full log: `fixture-tests-2026-10-10.txt`. The independent reviewer directly reran this same test executable: 79/79 passed.
- `cargo clippy -p safeupdate-core --all-targets -- -D warnings`: passed against the final source, including all post-review guards. Full log: `clippy-results-2026-10-10.txt`.
- `cargo clippy -p safeupdate-core --all-targets --features fixture-harness -- -D warnings`: passed. Full log: `fixture-clippy-2026-10-10.txt`.
- `cargo fmt -p safeupdate-core -- --check`: passed against the final source.
- `cargo tree -p safeupdate-core --target x86_64-pc-windows-msvc --edges normal,build`: recorded; 150 unique active external dependencies, with notices preserved.

## Final public-bootstrap extension rerun

- Default feature set: 85/85 tests passed; `bootstrap-tests-2026-10-10.txt`.
- Debug fixture feature: 87/87 tests passed; `bootstrap-fixture-tests-2026-10-10.txt`.
- Independent reviewer rerun of the final fixture test executable: 87/87 passed, including the historical-anchor lifecycle correction.
- Strict all-target Clippy passed both default and fixture-feature modes; paired `bootstrap-clippy-2026-10-10.txt` and `bootstrap-fixture-clippy-2026-10-10.txt` logs.
- Formatting check passed. No dependencies or production trust files were added.

The eight new tests cover both exact 4-key and 6-key profiles, fixed build pins, root length/hash/version, public-only strict fields, duplicate fields/keys/roles/signatures, shared keys, bad thresholds/signatures, root freshness and inability to reuse existing state under a different bootstrap anchor. Three lifecycle regressions also confirm historical expired-anchor rotation and saved-chain reopen, missing/invalid/expired rotation rejection, and preservation of metadata/release rollback floors. Fresh provisioning still rejects expired roots. Ephemeral test material never enters `bootstrap/`; its compiled bundle constant remains `None`.

## Post-review regressions

The final run includes the guards/tests for selecting from a superseded `TrustedRepository` after a newer refresh and a clock rollback between selection and download. All 77 tests passed after these edits, the saved-state validation changes and Windows write-through implementation, independent signed-model protocol and resumable-pause checks.

## Actual blockers / unexecuted claims

- A Windows-MSVC cross-check was attempted and failed in native cryptographic dependency builds before reaching this crate: the Linux environment lacks the MSVC C compiler, `lib.exe` and NASM. This is not a passing Windows build or test.
- The release link-size probe initially ran out of shared disk space. Its retry was cancelled at the build coordinator's request while disposable build artifacts were reclaimed. The optional probe was then skipped to reserve disk for the full application build. No completed component or Windows package-size measurement is claimed.
- No request was made to the user's IP update service. There is no embedded production root, no provisioned/verified IP HTTPS endpoint and no code-signing certificate in this source build.
- No Windows UI, Authenticode, ACL, installer, crash/power-loss, health-rollback or live proxy/TLS behavior was executed by this component's tests. Those are platform/release gates.

## Test coverage

TUF fixtures use in-memory ephemeral Ed25519 private keys. Tests verify real signatures and cover wrong role keys; payload/signature corruption; unknown signed-field tampering; duplicate-signature threshold abuse; root old+new authority, exact sequential rotation and skipped roots; root progress on later failure; timestamp/snapshot/targets binding; replay across state reopen; equal-version equivocation; future issuance, expiry and excessive validity; missing or oversized lengths; integer overflow; traversal; forbidden remote command/approval fields; application/model separation; compatibility; release rollback/equivocation; persisted quarantine and monotonic health rollback; clock rollback; corrupt/missing state; locks and symlinks.

Downloader fixtures cover HTTPS/IP/origin policy; redirect downgrade and hop caps; HTML/encoding rejection; exact streamed length/hash and overflow; cancellation/read deadline; strong ETag and exact Content-Range resume; changed/missing ETag; full-response restart; corrupt partials; unsafe local paths; and real TLS client construction. In-memory HTTP fixtures are not a live TLS interoperability test.

## Release values must come from actual artifacts

The current application runtime exposes worker protocol **1**. Test fixtures deliberately use protocol **2** to exercise compatibility; the design examples are illustrative. The publisher must generate actual release metadata from the final binaries, lengths, hashes and protocol constants. Fixtures, sample versions, keys and package bytes must never be published as a production repository.

## Independent model update extension

Added 10 authentic signed-metadata tests for independent app/model sequences and quarantine; exact content/schema/preprocessing/runtime/worker/source/license/manifest/expanded-byte compatibility; model replay/equivocation; immutable model paths; ZIP/expanded-size caps; forbidden approval/quality/command fields; and activation-time revalidation. One additional downloader test cancels mid-stream with a strong ETag, verifies the partial is retained, then uses a fresh token and exact Range/If-Range to finish. No new core dependencies were introduced. The activation ZIP extractor/self-test loop is tested separately by the application module; these core tests do not claim to have activated a model.
