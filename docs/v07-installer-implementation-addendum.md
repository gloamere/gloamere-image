# v0.7 installer durability implementation addendum (2026-10-10)

This implements the approved design's bounded, current-user Setup route. The latest user decision permits package authentication through provisioned TUF metadata without buying an OS code-signing certificate, and requires a real IP HTTPS origin. No root keys, live endpoint, server access, deployment, OS security bypass, or production auto-install activation are introduced here.

## Native recovery contract

- `gloamere-image.exe` remains the stable entry point; UI and worker move to `runtime/`. The root is copied only during an owned v0.6 bootstrap or a clean install. Ordinary updates never replace it.
- Setup extracts the native recovery helper and a bounded, build-generated file plan. It owns the same per-user maintenance and session locks as the launcher. Native commands verify the calling parent process. The outer updater releases its locks before Setup reacquires them; a competing process causes deferral.
- A single native installer journal in `%LOCALAPPDATA%/LightImageTool/updater-state` owns durable exact-byte file, raw registry, shortcut, and permission snapshots. Prepare publishes no mutation authority until backups have been flushed, verified and the prepared state reread. Begin is durable before Inno may write. A durable pending-health record retains snapshots after Setup returns. `ssDone` is not health confirmation.
- The file plan is embedded in Setup, never downloaded as a command. It permits only fixed product paths and third-party notices, carries target hashes and sizes, and records the previous owned manifest before overwriting. Foreign or altered paths are never overwritten. The installer refuses reparse paths, locked targets, competing sessions, unresolved prior transactions, insufficient space, pending reboot operations, and ambiguous ownership.
- Restore uses verified preimages and a durable write-ahead intent for already-owned paths, including torn/missing old files. Previously absent paths require declared/validated new bytes; unsealed new uninstaller files or unrelated registration/shortcut changes preserve evidence and require repair. The bootstrap root is restored last, after all other files and integration effects read back as old, so a power interruption cannot remove the recovery-capable entry too early. A failure to restore every covered effect is never reported as rollback success.
- Rerunning Setup first reconciles prior preparing/prepared/installing/rolling-back journals under both locks using the verified extracted helper. It does not bypass pending health, explicit repair, foreign changes, or ambiguous reboot operations. This handles interruption before the first root swap, while the old v0.6 entry remains intact.
- Bootstrap recovery may need to replace the currently running root launcher. Recovery copies a verified helper outside the installation directory, starts it with a process handle/identity handshake, and exits before replacement. An interrupted ordinary update is recovered before UI launch. Healthy UI acknowledgement checks expected version, package identity and protocol.
- User settings and AI records are outside the file plan. Update settings use the separate compatible file. No forced close, forced restart, delayed rename/restart replacement, or recursive cleanup is permitted. Cancel before begin discards the pending action safely; cancel after begin requests recovery rather than killing Setup.

## Verification boundary

Linux fixtures exercise executable native state-machine code with controlled filesystem and simulated integration adapters. Generated Inno contract tests cover the fixed calls, stable launcher guards, and staging layout. Native Windows installation, process/lock semantics, raw registry/security descriptors, SmartScreen behavior, actual power interruption, and v0.6-to-v0.7-to-next-release lifecycle remain explicit release gates. Compilation, static assertions, and fixture success do not satisfy these gates.

## Post-health bounded retention addendum (2026-10-10)

After durable healthy commit, retain the active recovery set and its exact previous terminal set. A backward-compatible journal link records the predecessor at prepare, rather than guessing from file timestamps. Snapshot file/directory permissions are recorded when the prepared snapshot is published and checked before retirement. Older journals without the link or permission baseline remain untouched. Pending, repair-required, another installation, unknown files, changed hashes or permissions, links, user settings/models and public evidence are never retirement targets.

Older terminal sets are retired using a private durable cleanup manifest outside their directory. It records only exact journal-derived owned paths, file identities, permissions and empty-directory identities. Every individual deletion is preceded by durable intent and checked again; no recursive delete is used. The journal and owner marker remain until backup deletion finishes and a durable finalization intent is published. Cleanup receipts remain to resume interruptions, including deletion replay after power loss. They contain identities and permissions, not backup content. Receipt storage is capped at 128 receipts and 16 MiB. Reaching either limit defers further retirement and records a bounded private warning; it never deletes receipts blindly or compromises healthy launch. Missing files are accepted only after that exact path has a durable deletion authorization, including on replay.

Cleanup is best effort under the maintenance lock after healthy commit and on subsequent committed startups. A failure retains evidence and a separate warning without changing the healthy transaction phase, rejecting UI admission, or rolling back the successful update. The predecessor may be a rolled-back attempt: the current successful transaction independently snapshots the actual previous healthy payload. A healthy A → failed B/rollback A → healthy C fixture reconstructs A from retained C preimages while leaving healthy C committed. Existing disk-budget checks still apply before each installation. This policy and its real Windows deletion/durability behavior remain part of native release acceptance.

Uninstall checks a bounded owner/hash stamp before executing its copied stable helper. The native helper then validates every exact installed layout file before allowing Inno removal; changed payloads stop uninstall instead of silently deleting them. Pending transactions continue to block uninstall.

## Official API reference checks

Checked the documented Setup event ordering (`ssInstall`, `ssPostInstall`, `ssDone`), cancellation directive, reboot directive, and temporary extraction semantics against Inno's current references. This is a documentation check, not an Inno compiler pass:

- https://jrsoftware.org/ishelp/topic_scriptevents.htm
- https://jrsoftware.org/ishelp/topic_setup_allowcancelduringinstall.htm
- https://jrsoftware.org/ishelp/topic_setup_restartifneededbyrun.htm
- https://jrsoftware.org/ishelp/topic_isxfunc_extracttemporaryfile.htm

Script error paths use suppressible product messages. A pre-install callback failure uses Inno's documented silent `Abort` at `ssInstall`; a post-install verification failure forces custom exit 20 and leaves native recovery enabled through deinitialization. No successful Setup exit or `ssDone` callback is treated as health. Reference: https://jrsoftware.org/ishelp/topic_isxfunc_abort.htm
