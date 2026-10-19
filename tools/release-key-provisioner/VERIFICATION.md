# Offline provisioner verification — 2026-10-10

## Passed

- Python 3.12, cryptography 50.0.0: 17 unittest cases, no skips with the Rust bridge configured.
- Rust 1.96.0: standalone debug bridge build, `cargo clippy -- -D warnings`, and `cargo fmt --check`.
- Encryption/decryption round trip of six ephemeral Ed25519 PKCS#8 private keys, wrong-password rejection, public-only output, and read-only recovery verification.
- Default dry run creates no files and never calls key generation; arbitrary production destination is refused before key generation.
- Redirected password input and getpass echo fallback are refused; no password command-line option exists.
- Existing empty/nonempty destinations, traversal, symlink ancestors, unsafe POSIX parent permissions, non-profile canonical input, and no-clobber writes are rejected.
- Simulated second-private-file write failure leaves no public root or completion marker and blocks reuse of the partial destination.
- Recovery rejects a private file changed to mode 0644; a mocked Windows DACL test rejects an Everyone grant on an individual encrypted private file. This mocked case is not native Windows verification.
- Eleven Rust bridge scenarios: valid Python-generated root and signed repository accepted; below-threshold signatures, corrupted signatures, root-body tampering, wrong bundle origin, wrong root length, private bundle field, expired root, future-issued root, signed private key field, and shared role key all rejected.

The bridge calls the public fresh-provisioning constructor `ProductionConfig::from_reviewed_public_bundle` and then `fixture::verify_fixture_repository`. It intentionally does not call the private historical compiled-anchor constructor. The core's separate historical-anchor path can allow an expired embedded root to reach authenticated sequential rotation without weakening this helper's fresh-root gate. The public constructor/schema contract is unchanged by that lifecycle split.

## Not established

- No production key was generated or imported, no user passphrase was received, and no live trust bundle was embedded.
- No server, TLS, certificate, port, SSH, scheduler, online signing service, or existing security permission was changed.
- No native Windows Python 3.11, DACL, reparse-point, MoveFileExW, removable-media, terminal-hidden-input, or power-loss test has run here. The host checks are Linux-only. These require disposable native tests before production use.
- No independent offline backup of production keys exists. Creation approval, a trusted user-local hidden prompt, separate custody, and an independent recovery drill remain prerequisites.
- This is not a completed publishing/refresh service, and the helper is not an application runtime dependency.

All test private material was generated only in memory or labelled temporary directories and removed by the test harness. No private key fixtures are part of the source deliverable. Python caches and Rust target outputs are excluded.
