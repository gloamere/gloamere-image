# v0.7 update-security implementation addendum (2026-10-10)

This addendum implements pages 7–10 of the approved Chinese v0.7 design. It precedes implementation. It does not supply production trust keys, an update origin, or a Windows publisher certificate. Production network/update functionality remains disabled until a real embedded TUF root and normally trusted IP HTTPS are provisioned. Authenticode additionally applies when the compiled policy requires it; see the confirmed unsigned-package adjustment below.

## Protocol and library decision

Use the maintained `tough` 0.24.0 Rust TUF verifier, with its safe expiration enforcement, sequential old-and-new-root threshold verification, timestamp/snapshot/targets verification and persistent datastore. Do not implement a look-alike JSON signature protocol or claim an unsigned manifest is TUF. Disable tough's default transport and supply a bounded HTTPS-only allowlisted transport. Restrict this repository profile to top-level targets and consistent snapshots; no delegations are needed in v0.7. Explicitly reject delegated metadata rather than silently broadening the supported release surface.

Official references checked 2026-10-10:
- https://docs.rs/tough/0.24.0/tough/ and https://docs.rs/tough/latest/tough/struct.RepositoryLoader.html
- https://github.com/awslabs/tough (MIT OR Apache-2.0)
- https://aws.amazon.com/security/security-bulletins/2026-019-aws/ (affected <=0.21.x; fixes >=0.22.0)
- https://theupdateframework.github.io/specification/latest/

## Defense in depth and persistence

A stable, user-owned state directory holds both tough's verified metadata and a separate monotonic application state. A held OS file lock serializes updates. State writes use same-directory temporary files, file sync and atomic replacement; missing/corrupt existing state fails closed, with first-time initialization separate from reopen. Record highest root, timestamp, snapshot and targets versions, their signed-body identities, and the highest verified release sequence per product/platform/channel/kind. Reject equal-version metadata/release equivocation as well as lower versions. Refresh and selection persist floors before handing out an install-capable descriptor. A health rollback affects installed/active state only; it never reduces these anti-rollback floors. Storage rollback by a same-user malicious process, administrator or disk snapshot is not preventable by an ordinary local file; document this limit, do not claim TPM-level monotonicity.

Keep the latest verified root as trusted state, together with the compiled root identity. Reopen validates state consistency. Root rotation is never reconstructed by trusting an arbitrary local JSON file. A corrupt local datastore is not reset automatically. Clock rollback relative to the last accepted wall clock pauses updates. Reject future issue times and unreasonable validity windows in the profile, in addition to TUF expiration checks.

## Release contract

Target names are strict relative ASCII paths with no percent escapes, backslashes, dot components, empty components, URL syntax, drive letters or query/fragment. App targets have the fixed layout `releases/{platform}/{channel}/{release_id}/Setup.exe`. Signed custom fields bind product, app/model kind, version, release sequence, platform, channel, current-user scope, updater/launcher/worker protocols and settings compatibility. Signed target length and SHA-256 are mandatory and bounded. No command, executable arguments or installation destination can be selected by metadata. Models remain a separate compatibility domain; repository signatures and `approved` booleans never certify model quality.

Return only opaque verified target descriptors after complete TUF + profile + floor verification. Download verifies full length/hash; Windows platform code must enforce the compiled package policy immediately before execution, under the install lock: exact system-trusted Authenticode publisher when configured, or the explicitly approved TUF-authenticated unsigned policy. A present invalid/unexpected Authenticode signature never falls through to unsigned acceptance. The descriptor is not itself permission to install.

## Transport and resume

Use HTTPS with normal certificate validation and exact reviewed allowed origins. Validate every redirect, with a small finite redirect count. Bound metadata bytes even when signed lengths claim huge values. Limit connection, idle-read and total duration; honor cancellation during requests, streaming and local hashing. Reject HTML and encoded bodies. Resume binds a partial file to target identity, expected length/hash and strong ETag; validate Range/If-Range and exact Content-Range. A changed object or full 200 response restarts safely; never append mismatched bytes. Final length/hash and the compiled platform executable-policy checks remain mandatory.

## Verification and limitations

Use ephemeral in-memory Ed25519 signing keys for authentic signed TUF test fixtures. Test wrong keys, corruption, replay/equivocation, metadata mix-and-match, expiration/future times, sequential root rotation/skipped roots, missing/overflow lengths, traversal, stream overflow/underflow, wrong range, changed ETag, HTML/non-TLS redirects, cancellation and persistent floors across reopen/health rollback. No fixture private key enters a production artifact. Tests do not establish Windows crash recovery, live TLS interoperability or power-loss durability; those remain separate platform release gates. Record measured dependencies and build size rather than claiming a package-size ceiling was met.

## Confirmed source-build adjustment

The user subsequently confirmed a direct IP distribution address (175.24.190.175) and no code-signing certificate. The compiled executable policy can therefore explicitly select `TufAuthenticatedUnsigned` for this build. This is not a remote downgrade switch. TUF signature/hash verification and normally trusted HTTPS with an IP-address SAN remain mandatory. If an executable has an invalid or unexpected existing Authenticode signature it must not fall through to unsigned acceptance. Missing embedded root or functioning trusted IP HTTPS still disables production updates. The UI must say unsigned and retain Windows security prompts. No certificate, TUF private key or server configuration is generated or changed by this work.
