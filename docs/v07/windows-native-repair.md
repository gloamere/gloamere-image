# Directory DACL restoration: native review repair

This design precedes the repair. Scope: new QA-owned NTFS fixtures and isolated v0.7 source only. No existing user installation, parent directory, system ACL, registry reboot state, or production trust material is changed.

Microsoft documents that SetNamedSecurityInfo/SetSecurityInfo impose inheritance on descendants. A directory snapshot records other entry names, not their security descriptors, so propagation can affect outside entries without detection. Reproduce with an unrelated leaf and nested descendant carrying a deliberately different inherited ACE, then record every owner's DACL before/after setting the same directory DACL.

Implementation: verify current ownership; compare the captured owner+DACL SDDL before mutation and return on exact equality. A changed directory descriptor cannot be restored using a propagating API: reject with Repair before any write and retain transaction/snapshot evidence. A leaf file may use the inherited-bit repair already validated, but pin the leaf using a non-delete-sharing file handle, reject directories/reparse entries, recheck the descriptor and ownership while pinned, and require exact post-write equality. Directory rejection is a deliberate automatic-repair limitation, not successful rollback. No enumeration-and-reapply workaround is acceptable because it could overwrite concurrent foreign changes.

The leaf mutation uses SetSecurityInfo on the pinned file handle, not a path-based setter. The official documentation advises against SetKernelObjectSecurity for file-system objects. Native probe shows modern setters add AI to non-AI preimages; therefore changed non-AI leaf descriptors also fail before mutation. Exact unchanged legacy descriptors still succeed as a no-op. This narrower automatic-restoration support is explicit; it does not rewrite legacy snapshot state.

Native regressions: unchanged directory with unrelated leaf and nested leaf leaves all descriptors unchanged; changed directory fails before any directory or descendant mutation; actual changed inherited leaf restores exactly; changed legacy descriptor fails without mutation; reparse/directory replacements cannot enter the setter. Preserve the unsafe API reproduction as evidence. Canonical full Debug/Release gates and resource packaging rerun on a new clean dev commit.

Source: https://learn.microsoft.com/en-us/windows/win32/secauthz/automatic-propagation-of-inheritable-aces (accessed 2026-10-10).

# Persisted experimental v0.7 target identity compatibility

The original unpublished v0.7 code used noncanonical JSON target identities. Random map ordering made equal signed content receive different hashes. The native fix canonicalizes the complete signed target and name. Production Config remains None, and no production trust state is provisioned or changed.

Persisted old experimental identities are not silently migrated or reset: a different identity at the same sequence must fail closed. An independently signed larger sequence may advance normally; no persisted metadata or release floor may be lowered or removed. This is an explicit limitation for unpublished experimental v0.7 test-state users, not an authorized reset procedure.

Before adding the regression: seed a valid ephemeral fixture state with a deliberately different legacy identity at sequence 71, persist and reopen; same-sequence selection must reject while bytes/floors remain intact; attempted floor replacement/removal must reject. Reopen again to prove persistence. No real user keys, server, install state, or rollback floor is accessed.

# Native blocked-input cancellation gate diagnosis

Release02 Debug stopped because the fixture's 6-second marker wait expired before observing the worker's blocked-input readiness. Preserve this failure and do not claim cancellation success. Product timeout (20 seconds), resource bound and post-request cancellation response (750 ms) remain unchanged.

First rerun the exact already compiled native test binary, without a rebuild or source mutation. Observe only its own process subtree and its new synthetic fixture under the private QA temp root. Record helper-copy, child-start and marker-readiness times and natural test exit; no window control or unrelated process access. This distinguishes worker startup failure from preparation/CPU timing. Any repair must follow demonstrated cause. A test fixture may use a smaller payload only if actual pipe bounds and peer readiness still demonstrate blocked IPC; it cannot weaken product timeout/cancellation assertions. Re-run targeted fault cases and then final canonical gates on a new clean commit if source changes.

Exact unchanged native rerun passed: all seven faults and real SDK recovery, with blocked-input total 2904 ms. The observer hit NoSuchProcess after the test naturally exited; the actual test's complete passing log is retained and the observer failure is not promoted into timing evidence. The original failure remains. This demonstrates a timing-sensitive fixture; the exact cold-start cause is unproven.

Native CreatePipe(size=0)/GetNamedPipeInfo probe reports 4096-byte input and output buffers. Use a 2 MiB blocked-input payload (512 times the measured buffer) instead of unnecessary 32 MiB preparation. Keep real peer-ready assertion, 150-ms wait for blocked writes, cancellation post-request <750 ms, total <6 s, hang timeout 20..23 s, resource cleanup and SDK recovery unchanged. No production code, timeout, hash policy, IPC bound or cancellation semantics changes. This does not establish a 32 MiB preparation performance budget. Record the payload and peer-readiness measurements in test JSON for both final profiles.

Official pipe semantics: https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createpipe
