# Ancestor redirection mitigation proposal — not implemented or reviewed

Actual Windows10 19045 NTFS evidence is in native-reparse-matrix.json (48 cases)
and native-reparse-preopened.json (8 cases and child-create control). Ordinary
FSCTL_SET_REPARSE_POINT and EX both succeeded on empty private directories with
exact0x2,0x100,0x110 access. Current share3 LIST_DIRECTORY pins did not stop them.
Read-only share1 stopped new0x2 handles and refused a pin when such a handle was
already open, but did not stop0x100/0x110. Protected-child directories failed145.
WRITE_EA0x10 failed5. Existing OS ancestors retained identity and OWNER+DACL.

These are restricted-access handles under the existing elevated test token,
without privilege adjustment, rather than impersonated untrusted users. The
result establishes a capability counterexample to the create-only explanation,
not a complete hostile multi-process scheduling exploit against the updater.

The smallest proposal worth independent review is:

1. Keep rejecting foreign WRITE_ATTRIBUTES0x100, DELETE_CHILD, DELETE,
   WRITE_DAC, WRITE_OWNER and generic write/all. Do not exempt host0x110.
2. Change ancestor pins in both Rust and Inno to read-only sharing while holding
   FILE_LIST_DIRECTORY (plus existing read-security/read-attributes access).
   This addresses0x2 admission by failing closed if a writer is already open
   and excluding new such writers during the pin lifetime.
3. Keep private product/state/temp/lock creation explicit-owner protected.
   Prove missing-component creation, pre-existing writers, all launch paths,
   pin lifetime through publication and child creation with the real candidate.
4. Audit the0x4 exception separately; directory mask aliases and IOCTL rights
   must be checked, not classified safe solely by the word create.
5. Preserve type/reparse checking after open and reject an object redirected
   before admission or between launches. Do not rely on share pins surviving
   process exit. A protected child invariant would require separate lifecycle
   proof before any broader exception for nonempty ancestors.

No production pin or mask has been changed in this stage. No real profile ACL
repair is qualified: changing the existing five product objects cannot resolve
the profile ancestor0x110 or reboot-reader blocker. A fresh controlled Windows
environment with compatible ancestors is an acceptance option; changing the
current real profile requires a separately reviewed, explicitly authorized
scope. The v0.7 candidate remains a development snapshot, not a release.
