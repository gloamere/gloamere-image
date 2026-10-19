# Isolated installer-preflight correction, 2026-10-10

This is an uncommitted candidate based on delivered source metadata commit 5981f9e and tested executable source fdcf0f3. It does not change the delivered archive, Windows state, existing installation, ACLs, registry, production trust or reboot policy.

## Actual evidence and diagnostic correction

The user's two Setup logs show reconcile returned 1 before prepare. The inspected updater-state contains only two empty lock files, no active pointer, journal, snapshot or last-error file. Its owner is BUILTIN\Administrators rather than the Setup process TokenUser; the DACL additionally permits other-principal writes. These read-only facts and the source guard explain an initial state-security refusal. Helper stderr was not captured, so this remains a grounded guard-path inference rather than a recovered exact stderr message. It is not evidence of an earlier installed candidate awaiting health. The independent oversized PendingFileRenameOperations gate remains later in the pipeline.

Typed owner and unsafe-write failures now produce fixed helper exit statuses 41 and 42. Inno decodes these only after Exec succeeds, never from a failed-launch Windows error. The diagnostic channel is numeric and bounded; it does not require writing or reading a file in the rejected directory. Existing strict owner and ACL checks are unchanged. No raw error text establishes recovery authority. Generic exit 1 remains generic, and a stale last-installer-error file is never presented as current evidence.

## Journal and preparation safeguards

The loaded journal installation must equal both the active pointer and requested root before namespace pins or recovery. Exact path mismatches still reject; no case-folding, alias acceptance or relocation is introduced. Initial reboot-state refusal occurs before a new snapshot directory or Preparing pointer is published. The post-snapshot and immediate pre-install checks remain. Errors after snapshot preparation still retain evidence and permit only the existing no-write-phase reconciliation.

## Protected creation and pre-lock admission

TokenUser and the default TokenOwner differ. Under an administrator token, default creation can produce Administrators ownership. Inherited permissions may also be broader than the updater permits. The strict current-user owner and writer rules remain; Administrators ownership is not adopted as an alternative.

The candidate adds shared read-only Win32 owner/DACL checks on root-to-leaf pinned handles in Inno before Setup or uninstall creates/acquires lock files. This also validates the state before uninstall trusts its launcher hash stamp. State creation uses the fixed LocalAppData product path; missing owned directories and new lock files receive an explicit TokenUser owner and protected user/SYSTEM/Administrators DACL at creation. Existing objects are inspected without changing their security descriptors. Lock opens use OPEN_REPARSE_POINT, no truncation, metadata/type/link-count checks and handle-based security checks before locking. Launch failures, pre-lock security refusals and child exit codes remain distinct.

Rust has equivalent private-tree, exclusive-directory, lock and temporary-file primitives. Fixed update-root callers resolve the OS Known Folder rather than accepting a state root from CLI or settings overrides. Existing OS ancestors need trusted ownership and no foreign destructive/reparse-capable rights; app-owned components additionally require exact TokenUser ownership and the strict writer allowlist. Ancestor FILE_DELETE_CHILD/WRITE_DAC/WRITE_OWNER checks protect the persistent namespace between process runs as well as while handles are pinned. New temporary files use atomic CREATE_NEW with explicit security rather than creating broad files and rewriting them afterward. Atomic journal writes, native backup/helper copies and exclusive transaction directories use these primitives.

A racing existing path is audited, never repaired; exclusive transaction collisions reject. A group-owned, broadly writable, reparse, multiply-linked lock or ambiguous path stays refused. No token default, elevation, privilege, existing ownership or existing ACL is changed. The existing user directory still requires separately approved, scoped permission repair; this source correction does not perform that repair. No insecure fallback log is introduced.

Scope remains bounded. This is not a claim that every existing administrator-owned payload can migrate or that every model/download creator has changed its default owner. In particular, the independent model-root initializer and core transport have their existing creation behavior. A full admin-token prepare/install/health/retention/rollback acceptance run is required: root creation and portable tests alone cannot prove that lifecycle. Existing-file permission restoration and its repair-required cases are unchanged.

## Required native regression matrix

- Standard user, split administrator token, elevated administrator token and built-in Administrator: absent directory creation with explicit owner/DACL and exact readback; no unrelated parent/child ACL changes.
- Existing private directory reuse; existing Administrators-owned or other-principal-writable directory refusal with byte/security snapshots unchanged.
- Null DACL, unsupported ACE, foreign ancestor FILE_DELETE_CHILD/WRITE_DAC/WRITE_OWNER, junction/ancestor swap, racing creator, nonempty or hardlinked lockfiles, denied ownership query and unsupported filesystem: fail closed.
- Complete admin-token prepare/install/health/retention/rollback, including every newly created journal, backup, helper and payload identity. Verify existing-file metadata and unrelated paths remain unchanged.
- Exec false with Windows codes 41/42 remains a launch error; Exec true with helper 41/42 maps only its documented failure; no stale-file diagnosis.
- Initial reboot false/query error/oversize produces no new pointer or snapshot; late change preserves Preparing evidence without payload writes; final begin check still rejects newly pending operations.
- Pointer/journal/requested-root mismatch cannot reach recovery, including a matching-looking integration snapshot at another root.

Portable source tests and source-contract assertions do not establish native Inno execution, token/ACL behavior or successful installation. The actual 40 MB pending-rename state is still refused at the 64 KiB bound and must not be bypassed.

## Official references checked

- CreateDirectoryW explicit descriptor and inherited defaults: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createdirectoryw
- TokenUser, TokenOwner and default DACL distinction: https://learn.microsoft.com/en-us/windows/win32/secauthz/access-tokens
- Inno Exec launch-error versus process-exit contract: https://jrsoftware.org/ishelp/topic_isxfunc_exec.htm
