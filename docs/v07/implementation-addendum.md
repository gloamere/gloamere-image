# v0.7 implementation addendum — 2026-10-10

The approved 2026-10-10 DOCX is the authority. Work is on a copy of the latest v0.6 source, with all three native lint synchronizations independently hash-checked. Original source and Windows branches are unchanged. No push, live deployment, signing credential, user-image write or production service change is authorized here.

## Boundaries and interfaces

- AI owns independent component, execution and release-tier state, explicit experiment opt-in, bounded folder-only scheduling and honest experimental results. Existing component hashes and production qualification cannot be changed by settings or cache.
- `crates/update-security` owns authenticated TUF metadata, anti-rollback state and bounded download protocols. `src/updater` owns preferences, consent/cancellation state, persistent install/recovery journal, platform policy and stable launch protocol. Neither a hash-only metadata file nor a fixture certificate can enable production.
- `settings.json` remains schema 1. Updater choices live in `update-settings.json`; AI records and manual decisions remain separate and reversible.
- The stable `gloamere-image` launcher dispatches to `runtime/gloamere-image-ui` and is excluded from ordinary automatic replacement. Bootstrap installer migration is explicit. Recovery runs before UI; immutable snapshots stay until exact-version UI health is confirmed.
- Tests use isolated temporary directories and controlled fixtures, never the user's real installation or clipboard. User clarification at 00:16 UTC selects the direct server IP with no existing code-signing certificate. The build-time publisher policy may explicitly permit a TUF-authenticated unsigned EXE and shows that limitation; any present invalid or unexpected signature fails. Signed builds use system Authenticode trust plus an exact signer certificate identity. HTTPS must validate the exact IP SAN, with no HTTP or self-signed bypass. Missing provisioned trust roots, IP TLS or platform acceptance keeps production automatic installation disabled, including after opt-in.
- Cancellation is effective before file submission. During submission no process is killed. Normal exit, absence of unsaved work, drained workers and cross-instance locks are prerequisites. Shutdown/logoff is not authorization to start installation.
- Recovery preserves exact product-owned preimages, permissions and versioned integration metadata. Unknown/foreign files are never recursively deleted. Failed or interrupted transactions are quarantined; unclear reboot operations or impossible recovery require repair.

## Verification sequence

1. AI state/cache/scheduler and CPU UI regressions; unchanged known false-negative score remains a documented fixture.
2. Signature, replay, expiry, path, resume and package-replacement rejection tests.
3. Persistent journal fault injection at prepare, backup, commit, installation return and health; exact old-version restoration, foreign-file and data preservation, lock competition.
4. Formatting, strict workspace/all-targets lint, Debug/Release tests and installer packaging regressions. Static Windows cross-check when available.
5. Windows native acceptance is a separate gate: clean install, v0.6 bootstrap, second release, standard-user Windows 10/11, DPI and shell entry points, process/session ownership, real signing and OS protections. Cloud results are never reported as native or production release readiness.
