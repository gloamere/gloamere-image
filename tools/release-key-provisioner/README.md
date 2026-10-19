# Offline TUF release-key provisioner

Status: prepared operator helper, **not a production key set**. Production updates remain disabled. No server access, certificate creation, port, TLS, SSH, payment, app trust configuration, or deployment is performed by this directory. This is not an Authenticode certificate tool.

## Approval and custody come first

Before any production generation, obtain the user's explicit, action-time approval of all of the following:

1. There is no existing TUF root/key set to reuse. If one exists, stop: securely inspect its public root and use a separately reviewed rotation/reuse procedure. Never generate a replacement root and silently strand installed clients.
2. Create **six distinct Ed25519 keys**: three root keys with threshold 2, and one each for targets, snapshot, and timestamp with threshold 1. The alternative four-key, all-1-of-1 profile requires a separate decision and is deliberately not implemented by this helper.
3. Store private keys only as passphrase-encrypted PKCS#8 PEM files at the exact resolved local path `%USERPROFILE%\GloamereReleaseKeys\v0.7`. Resolve and show the actual user's path before creation. No private key, passphrase, or private directory may be sent to chat, Library, source control, build/source archives, the distribution server, or any other service.
4. The user runs the creation command and types a new passphrase twice into their **own local hidden terminal prompt**. An agent must never enter it, request it in chat, pipe it through stdin, store it in an environment variable, put it in a command-line argument, or record the terminal session. An approval message is not the passphrase handoff.
5. Only the public `root.json` and `public-bundle.json` pair is eligible for the separately reviewed application trust bundle and public delivery. Only signed public repository files are eligible for the separately authorized distribution server deployment.
6. The user controls offline backup media and performs a recovery drill before public integration. Loss of enough root keys or the passphrase can make trusted rotation impossible.

No existing ACL is modified by this tool. If a permission check fails, stop and report the precise path and required access change. Ask separately before any agent changes a security-sensitive permission. A flag or local prompt in this script does not replace required user approval.

### What 2 of 3 does and does not mean

The initial convenience vault holds all six encrypted keys under one passphrase. Compromise of that machine and passphrase exposes all three root signers. **That is not independent compromise resilience.** Three files in the same folder, or three copies on the same disk, do not establish separate custody.

Before launch, agree on independent offline custody for the root signers, ideally separately controlled devices/media and distinct protection. Any re-encryption, movement, removal, new custodian, or credential provisioning is a separate reviewed action. Keep root keys offline except for reviewed root rotation. Keep the targets signing key off the web server as well. Do not copy the entire convenience vault into CI or onto `175.24.190.175`.

## Optional operator environment

The main application has no Python/cryptography dependency. This helper is an optional operator-side Python 3.11+ tool. It uses maintained `cryptography` Ed25519 and PKCS#8 serialization rather than custom cryptography. The pin in `requirements-operator.txt` is the exact version tested here, not a claim of perpetual currency or an independent dependency security audit. Review the official package's current release/security information before first production use.

On the operator's own machine, prepare a separate virtual environment and install from official PyPI before taking the signing machine offline. These are operator instructions, not authorization for an agent to install software:

```powershell
py -3.11 -m venv .venv
.\.venv\Scripts\python.exe -m pip install --index-url https://pypi.org/simple -r requirements-operator.txt
.\.venv\Scripts\python.exe provision_keys.py
```

The final command is a read-only dry run: no key generation or file creation. Review/install dependencies before disconnecting; key creation and recovery contain no network calls.

The user must first create the `%USERPROFILE%\GloamereReleaseKeys` parent as an ordinary local, user-owned private directory. Do not use synced Desktop, Documents, OneDrive, Dropbox, shared folders, a repo, or a network drive. The script checks the parent and each created directory. Windows checks ownership and DACL grants read-only through built-in PowerShell, permitting only the current user, SYSTEM, Administrators, and an inherit-only Creator Owner grant. Unfamiliar grants cause refusal, including broad group grants. No ACL is silently repaired. POSIX requires user ownership and no group/other bits.

After explicit approval, on the user's own local terminal:

```powershell
.\.venv\Scripts\python.exe provision_keys.py --create
```

The user must type the displayed creation sentence and enter the passphrase twice. Use a unique high-entropy password-manager-generated passphrase, at least 16 characters, and save it in the user's own approved password manager separately from the offline backup media. The tool does not save passwords. It rejects redirected input and `getpass` echo fallback. Python cannot guarantee secret memory zeroization or protection against a compromised operator account, administrator, keylogger, terminal recorder, crash dump, or swapping. A local trusted operator environment and OS disk protection remain necessary.

## Output and failure behavior

```text
%USERPROFILE%\GloamereReleaseKeys\v0.7\
  private\
    root-1.encrypted.pk8.pem
    root-2.encrypted.pk8.pem
    root-3.encrypted.pk8.pem
    targets.encrypted.pk8.pem
    snapshot.encrypted.pk8.pem
    timestamp.encrypted.pk8.pem
  public\
    root.json
    public-bundle.json
    public-audit.json
  COMPLETE.json
```

Private files are encrypted before any disk write. Each file is written to an exclusive `.pending` file and flushed. On Windows, same-directory `MoveFileExW` without replacement and with `MOVEFILE_WRITE_THROUGH` publishes its absent final name; on POSIX, a no-clobber hardlink followed by unlink and directory fsync does so. An existing entry is never replaced. All six encrypted files must be persisted and successfully decrypted/read back against their public keys before any public root is emitted. A completion marker is written last. The root is standard TUF 1.0 metadata with consistent snapshots, all three initial root signatures, and signed `issued`/`expires` fields, valid for 365 days. Six key IDs are SHA-256 of the restricted ASCII canonical public key objects. Canonicalization deliberately rejects arbitrary strings, Unicode, floats, and escapes; this is not a general-purpose metadata editor.

The helper refuses a pre-existing destination, even if empty; traversal, network/device paths, links/junctions/reparse points, an unreviewed parent, or an unsupported no-clobber filesystem also fail closed. Failures can leave an encrypted partial vault or `.pending` file. **Do not use it, delete it automatically, rerun into it, or silently make a new root.** Inspect locally and decide recovery with the user. No public output is eligible without `COMPLETE.json` and the recovery checks. Initial provision is not transactional across a power loss. Windows directory durability and filesystem/ACL behavior have not yet been verified on the target machine; the Win32 write-through call is not a claim of tested power-loss safety. Run disposable Windows filesystem and ACL checks, then verify an independent decryptable backup before any production integration. PowerShell is resolved through `GetSystemDirectoryW`, never PATH or the current directory.

The tool assumes a trusted user-owned parent not concurrently modified by another process. It does not claim to defeat a same-user attacker racing filesystem operations or an administrator. Do not run it in an actively hostile account.

The public bundle binds product, Windows x64 stable channel, the exact origin `https://175.24.190.175`, approved TUF-authenticated unsigned package policy, six-key profile, root version, exact length, and SHA-256. It does not prove that server's HTTPS is working. App activation still requires reviewed compile-time public trust embedding and normally trusted HTTPS with the IP address in the certificate SAN.

## Offline backup and recovery drill

1. With the user's authorization, copy the encrypted vault to user-controlled offline media without involving cloud sync, Library, chat, or the server. Preserve private ownership/access. This helper does not copy, upload, delete, or change permissions.
2. Record the public root SHA-256 and key IDs through a separate user-controlled audit channel. Verify these against the public files before embedding.
3. Safely disconnect the original source. On a trusted local machine, test the backup directly with the read-only command below. It decrypts only in memory, confirms every role/key mapping, verifies root signatures and the public hash/length manifest, and writes nothing. Reconnect only what is necessary for the approved workflow.

```powershell
.\.venv\Scripts\python.exe provision_keys.py --verify-backup --vault "E:\ApprovedOfflineBackup\v0.7"
```

4. Enter the same passphrase locally in the hidden prompts. Compare the reported public root SHA-256 with the separately recorded fingerprint. A wrong password, mismatched file, different root identity, missing completion marker, unsafe file, or test-key marker must fail. Each private file's own owner/access is checked, not just its containing directories. The drill establishes key recoverability and identity only; it deliberately does not establish full fresh-bootstrap acceptance or reject an old root solely for being expired. The separate strict Rust provisioning verifier remains authoritative for public root/bundle schema, lifetime, version, and policy acceptance. A successful local drill does not substitute for that check or independent root custody.
5. Store backup media offline and separately. Do not destroy the only decryptable copy. If moving root keys to separate custody, separately document which two custodians can authorize rotation and how recovery works. The all-six-key recovery drill applies to the initial full backup; it does not claim to validate a future split-custody arrangement.

## Publisher freshness is a separate launch gate

Generating a root does not run a publisher. The current client rejects expired or future-issued metadata and caps root lifetime at 366 days, timestamp at 2 days, and targets/snapshot at 31 days. Recommended operating windows: root 365 days; targets/snapshot 7 days; timestamp 24 hours.

Before enabling production updates, assign an owner and choose an explicitly authorized maintenance process. A reasonable design is to re-sign timestamp at least every 12 hours with an incremented version and current `issued`, updating the exact length/hash of the selected signed snapshot; reissue targets/snapshot before their 7-day expiry, even when binaries have not changed, and increment changed metadata versions. Upload new content-addressed targets and versioned metadata first, then replace timestamp last. Preserve old root versions and sequential dual-threshold root-rotation history. Keep the active metadata chain internally consistent and run the real client verifier before publication.

Monitoring should alert sufficiently before expiry and verify HTTPS, exact metadata bytes, signatures, roles, hashes, lengths, and expiry from the public endpoint. If maintenance stops, updates fail closed; normal application use must not depend on refresh. A background refresh job, standing signing authority, new online signing environment, server key custody, deployment credentials, scheduled automation, TLS configuration, or port changes **must be separately proposed and authorized**. This helper creates none of them. Never put root keys online to avoid designing timestamp operations.

## Verification

```text
python -m unittest -v test_provision_keys.py
cargo build --manifest-path verify-root/Cargo.toml --offline
# Set TUF_ROOT_PROBE to the resulting debug verify-ephemeral-provisioner-root binary,
# then rerun the Python suite. Do not build this test bridge with --release.
```

Every test key is ephemeral and held in a labelled temporary directory which is removed afterward. No private fixture is committed or integrated. The optional Rust bridge uses the actual `safeupdate-core`/`tough` verification path, with the existing debug-only fixture transport, and exercises valid and below-threshold/corrupted/tampered roots. It does not access a server. Test binaries, caches, and temporary files are not release deliverables.

Official references checked for this implementation:

- [TUF specification](https://theupdateframework.github.io/specification/latest/)
- [TUF roles and metadata](https://theupdateframework.io/docs/metadata/)
- [cryptography Ed25519 API](https://cryptography.io/en/latest/hazmat/primitives/asymmetric/ed25519/)
- [cryptography serialization and BestAvailableEncryption](https://cryptography.io/en/50.0.2/hazmat/primitives/asymmetric/serialization/)

`BestAvailableEncryption` is a maintained-library choice, not a fixed cipher/KDF promise made by this tool. Keep the operator package and its licenses separate from the shipped application; do not add it as an app dependency.
