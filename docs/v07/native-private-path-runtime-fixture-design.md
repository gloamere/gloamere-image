# Actual native private-path runtime fixture, before code

The candidate's unit tests replace native directory/tempfile/lock creation with
filesystem fixture adapters. Passing those tests does not prove shipping Win32
creation. Add a nonshipping example which compiles the EXACT private_paths.rs
source as its own module and invokes the actual library platform/storage APIs.
No production API, security policy, Cargo feature, KnownFolder override or
integration/registry backend changes. The example is never staged in Setup.

Input fixture root must be a fresh C:/Windows/Temp/gloamere-preflight-native-UUID
directory. Read-only C:/, Windows, Windows/Temp ACL audit found the ancestor
policy compatible; authorized task and D:/Temp ancestors are incompatible.
Execution requires an approved elevated command because the new temporary
directory is outside normal task write roots. Create ONLY this new uniquely
named directory/children with explicit TokenUser owner and protected DACL.
No setters, parent ACL changes, token privilege adjustment, real installation,
Registry, shortcut, KnownFolder contents, network or reboot action.

Exercise root-to-leaf admission and missing child creation, exact reuse, directory
exclusive collision, journal/active/helper/backup temp files through actual
production atomic storage/private-tempfile calls, new/reopened locks, hardlink
and nonempty lock refusal, unknown SID writer refusal on new synthetic object,
BA owner refusal on a new synthetic object, and cross-thread ancestor rename
blocked while actual directory pins are held. Capture by-handle owner/DACL,
control flags, object identity and content hashes. Unknown principal is not
resolved or guessed. Preserve unrelated sentinel bytes and security.

This is production Win32 primitive coverage, NOT complete Transaction::prepare,
actual Inno prepare/install/health/rollback/uninstall or a normal-user acceptance.
Those remain separate. The host real LocalAppData ancestry remains rejected.
The fixture never relaxes a parent check merely to pass. Preserve fixture and
JSON evidence for bounded cleanup/review; no recursive repair.
