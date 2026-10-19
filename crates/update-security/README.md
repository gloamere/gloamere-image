# safeupdate-core 0.7

A real Rust TUF verifier and bounded downloader for gloamere-image. It never runs an installer, loads a model, generates production keys or changes a server. The source-build production configuration is intentionally unavailable: `ProductionConfig::bundled()` returns `NotConfigured`. Knowing the direct IP address does not establish a trust root or provision trusted IP HTTPS.

For a separately authorized future build, `docs/PUBLIC-BOOTSTRAP-DESIGN.md` describes a public-only manifest/root pair and an explicit reviewed `include_bytes!` source edit. `from_reviewed_public_bundle` validates exact compiled product/scope/origin/publisher pins, root bytes, public-only role profiles and freshness; it never enables the current build or reads adjacent files/environment settings. Current `COMPILED_PUBLIC_BUNDLE` stays `None`. Existing state cannot be silently reset or rebound. Fresh provisioning rejects expired roots; historical immutable anchors may enter normal authenticated TUF rotation, with final trusted-root/metadata expiry and rollback checks unchanged.

## Integration contract

- Run blocking operations on a background thread, never the UI event loop. `CancellationToken` is re-exported; no direct Tokio dependency is required by the caller.
- `LockedState::initialize(path, config)` is a distinct first-install action. `LockedState::open` refuses missing/corrupt state. Never fall back from an open error to initialization. Preserve this directory independently of application snapshot/health rollback.
- The application supplies a private, absolute, current-user-owned state/cache directory. Windows platform code must verify its owner/ACL. No path ancestor, state, lock, partial or package may be a symlink/reparse point. Unix download-cache directories must explicitly use mode 0700.
- `blocking_check(config, &mut state, &compatibility, &cancel)` verifies TUF and returns an opaque `VerifiedTarget`. Compatibility covers installed release sequence, updater/launcher/worker protocols, readable settings schema and a stable local installation ID for rollout. The ID is not transmitted.
- `blocking_download(config, &target, destination, &cancel)` streams to a partial file, checks exact signed length/SHA-256 and returns a downloaded file identity. A raw `download::ExpectedTarget` is an integrity helper only, never an installation authorization.
- Immediately before execution, hold the platform install lock, call `revalidate_for_execution`, rehash a locked package handle and enforce the compiled `PublisherPolicy`. Only platform code may construct the fixed local installer command. Revalidation fails after metadata/release floors change, expiry, clock rollback or release quarantine.
- An exit helper can re-fetch metadata and select the exact release/hash/length expected by its pending plan. The pending JSON is an expectation, not authority. Do not deserialize/create a `VerifiedTarget` from it.
- After health failure, call `quarantine_app_release`. Recovering the previous healthy installation never lowers trust floors. Repair releases use a higher sequence. There is no generic signed-downgrade escape hatch.

## Independent installed-model updates

The model-update choice must be checked separately by the caller. It is not application-update consent, first-time model-install consent, experimental-screening consent or upload permission.

- `ModelCompatibility` is constructed from the compiled model allowlist plus local installed sequence/cohort. It is not deserializable from a remote manifest. Exact pins cover model ID/content version, schema, preprocessing, runtime/worker/source, the compiled pack fingerprint, license set/list, manifest SHA-256 and expanded byte count.
- `blocking_check_model` returns only an opaque `VerifiedModelTarget` for that exact profile. `blocking_download_model` downloads its bounded ZIP. Model sequences and `quarantine_model_release(model_id, sequence)` are independent of the application and other model IDs.
- `revalidate_model_for_activation` must run under the activation lock. The caller then performs exact archive inventory/path/expanded-size checks, pinned pack verification and synthetic self-test before atomically replacing its active-pack pointer. The old pack remains available on failure.
- A signed publisher cannot authorize a new graph or native library hash, or turn a model into a quality-approved/stable model. New code/model identities require a reviewed application allowlist update. Unknown approval, tier or command fields are rejected.
- Model path: `models/{model_id}/{model_version}/{platform}/{release_id}/model-pack.zip`, with SHA-256-prefixed basename on the server. The current model version is its weights hash. ZIP cap: 256 MiB; expanded-size cap: 512 MiB, further restricted to the exact compiled byte count.
- Pausing cancels only the active request and retains a valid strong-ETag-bound partial. Resume uses a fresh cancellation token. Explicit cancellation cleanup belongs to the caller after the downloader releases its lock. Invalid/integrity-failing partials are discarded; a server without a strong ETag cannot promise resumability.

For end-to-end tests only, the default-off `fixture-harness` feature supplies `fixture::verify_fixture_repository` with bounded signed metadata bytes. It runs the same verifier and produces real opaque authority; normal release builds reject this debug-only feature. No unchecked descriptor constructor exists.

See `docs/MODEL-UPDATE-DESIGN.md`. No production TUF root or IP HTTPS has been provisioned, so actual production model networking remains unavailable along with application updates.

## Repository profile

See `docs/SECURITY-DESIGN.md` for the pre-code addendum and `docs/download-design.md` for transport decisions. `tough = 0.24.0` performs real TUF cryptographic verification. This application deliberately accepts a stricter top-level-only, consistent-snapshot profile; it is not a general repository editor or certification of every TUF optional feature.

Each signed role includes an `issued` RFC3339 timestamp. Future issuance and expiration are rejected; maximum validity is 366 days for root, 2 days for timestamp, and 31 days for snapshot/targets. Metadata bodies are capped at 1 MiB, refresh at 90 seconds and 24 metadata requests, and root advances at 16 per refresh/128 since the embedded anchor. Mandatory snapshot/targets bindings include version, bounded byte length and SHA-256 even though generic TUF permits omitting some fields. Unknown signed fields remain covered by TUF signatures.

App targets are named `releases/{platform}/{channel}/{release_id}/Setup.exe`. On the server the consistent-snapshot artifact is `releases/{platform}/{channel}/{release_id}/{sha256}.Setup.exe`. App packages are capped at 64 MiB. General signed targets are capped at 1 GiB; model targets cannot be selected as applications or establish model quality. App custom fields follow `ReleaseMetadata` exactly; commands, install paths, publisher policies and `approved` flags are rejected there.

The only source-build executable-policy adjustment approved for the user without a code-signing certificate is the explicit compiled `TufAuthenticatedUnsigned` enum. That still requires TUF authenticity and normally validated HTTPS. Platform code must reject a present invalid or unexpected Authenticode signature, never downgrade it to “unsigned.” The signed-certificate policy pins exact certificate DER SHA-256. No server metadata can select either policy.

## Persistence and threat limits

The application state stores verified root-chain evidence, per-role highest signed-body identities, release identities, quarantine and the clock floor. State replacement uses a same-directory temporary, file sync and atomic replace; Windows uses `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`, Unix also syncs the directory. OS file locking serializes concurrent access. Authenticated root/timestamp progress is preserved even if a later role fails. Tough's non-atomic datastore is scratch only.

The checksum detects file corruption. It is not a MAC, TPM counter or defense against an administrator/current-user malicious process restoring the entire directory or disk snapshot. Same-user memory/process compromise is likewise outside the guarantee. Torn writes and actual power-loss behavior still require Windows release qualification. Corrupt state blocks updates and requires an explicit reviewed repair; it is never silently reset.

## Dependencies, builds and verification

Core license: MIT OR Apache-2.0. `tough` and its cryptographic dependencies come from crates.io; no vendored cryptographic implementation was rewritten. TLS shares AWS-LC with tough instead of pulling a second provider solely for HTTP. Reqwest uses native certificate roots, system proxy discovery and normal host/IP-SAN validation. Native roots are read at client construction; OS-specific revocation/PAC behavior still needs Windows integration testing.

`docs/DEPENDENCY-LICENSES-WINDOWS.txt` is the actual normal/build dependency tree; `docs/dependencies-windows.json` inventories 150 active Windows dependencies in this locked workspace. `third-party-notices/` preserves crate and bundled native-library licenses/notices, plus a few conservative superset notices. The typed-path crate omitted standalone license files, so its upstream README grant and the selected unmodified Apache-2.0 text are included. The complete application inventory is now regenerated and verified: 449 locked Windows package versions and 823 notice/declaration files; see the repository-root `docs/v07/dependency-notices.md`.

Commands (from repository root):

```
cargo test -p safeupdate-core
cargo clippy -p safeupdate-core --all-targets -- -D warnings
cargo fmt -p safeupdate-core -- --check
cargo build -p safeupdate-core --release --example size_probe
```

Tests use ephemeral Ed25519 keys and in-memory transport fixtures, with no production/private key files, live server, listener or certificate bypass. The link-size probe retains real check/download function paths and performs no network requests. Its Linux ELF size is a component measurement, not a Windows installer-size guarantee. See `docs/VERIFICATION.md` for results and outstanding Windows gates.
