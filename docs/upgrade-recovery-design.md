# Upgrade recovery acceptance extension

The real scene is a user upgrading an existing installation while retaining image
files and preferences. A recoverable error after the paired executables have been
replaced must restore the older runnable pair, manifest, uninstall registration
and owned Explorer values consistently. Enabling cancellation alone is not evidence.

An early README lock only tests a failure before the executable replacement and
does not establish this property. The isolated acceptance extension locks a later
third-party notice with an exclusive file handle, without filling the disk or
changing any system policy or unrelated software. The controlled setup runs
silently; the harness watches hashes of the known installed main and worker while
it progresses. Both must be observed matching the new validated payload before
the recoverable failure, with setup log ordering as supporting evidence.

After setup fails, compare every baseline payload hash, the full manifest hash,
uninstall values and all product-owned menu values against their snapshots.
Also verify the user image, preferences and default/UserChoice snapshots remain
unchanged. A failed or unobserved transition is recorded as such, never a pass.
If Inno's default recovery cannot satisfy this extension, introduce an explicitly
designed backup-and-restore or staged upgrade implementation before shipment and
repeat the test. Native wizard cancellation and abrupt termination remain separate
unverified cases; no guarantee of recovery from power loss is made.


The first actual install uncovered an Inno preprocessor scope error: all three
registrations expanded to .jpg, leaving .jpeg/.png absent. The corrected design
uses explicit entries for the three extensions, avoiding mutable preprocessor
state. Recompile both the private older-payload fixture wrapper and current setup;
the fixture is never presented as a previously released installer. Verify all
three keys through actual HKCU snapshots before testing recovery.


Actual evidence: the late notice lock allowed both new EXE hashes to be observed,
but the old main hash was not restored after setup failed. Default Inno recovery
is therefore insufficient for this product and this fixture.

The corrected implementation takes an explicit backup before the first payload
write. It snapshots every path the new installer can overwrite, all existing
Inno uninstaller files, and typed uninstall/product-menu values. This includes
the old manifest and the paired EXEs; unrelated image files and preference files
are outside the overwrite set. The compile-time validated path list is used,
without parsing an untrusted installed manifest or copying arbitrary user trees.

The backup resides in a dedicated product recovery directory within the selected
owned installation directory, with an ownership marker. Existing recovery data
causes a refusal, avoiding overwrite of evidence. File copies are SHA-256 checked;
backup failure aborts before payload changes. Registry snapshots support the
string and DWORD types produced by this setup; other types are refused before
writes. Known Explorer values are restored individually, preserving unknown
values and shared parents. The fixed uninstall namespace is restored to its
previous typed values; default associations remain untouched.

DeinitializeSetup restores the snapshot when setup did not reach ssDone, after
Inno's normal failure cleanup. Originally absent known files are removed, old
files are copied back and hash-checked, and newly created uninstaller files are
removed only when their names were absent in the snapshot. A successful setup
cleans its own verified backup; a recovery error retains the backup and reports
its path. The app is never force-closed. Concurrent user launches, permissions
preventing restoration, forced termination and power loss remain explicit limits;
retained recovery data is never silently discarded. Native cancellation is not
claimed verified. The late-file test must pass with old payload, registration,
menus, settings and image snapshots restored before this installer is accepted.


The corrected isolated lifecycle observed new main and worker bytes, then restored
all older payload hashes, typed uninstall values and menu snapshots after a late
notice write failure. Successful upgrade and final uninstall also passed with
user image/settings and foreign registry data retained. The final package repeats
this lifecycle with exact uninstall EXE/data hash comparisons as well.

Framework hooks and hash behavior follow the official references:
https://jrsoftware.org/ishelp/topic_scriptevents.htm
https://jrsoftware.org/ishelp/topic_isxfunc_getsha256offile.htm
https://jrsoftware.org/ishelp/topic_scriptfunctions.htm


## Review hardening before final freeze

A failed first installation can leave a generated uninstaller outside the
pre-install payload list. Each existing generated EXE/DAT/MSG must be removed
successfully; collect the exact failed paths and keep the recovery directory on
any failure. Never report full restoration or clean the backup after failed deletion.

Before restoration or cleanup, recheck the installation root, all ancestors,
each payload/backup path, marker and snapshot for reparse points. Preflight the
whole list before touching any file, and check again immediately before each
copy/delete/prune. Refuse a changed junction and retain recovery data with the
path in the report. This reduces post-backup path substitution risk; it is not
a guarantee against arbitrary concurrent replacement between checks and I/O.

Focused tests use the real included recovery functions in a small Inno fixture,
inside fresh project-owned D:/Temp directories. Hold a generated uninstaller
read-only (deny delete), and replace only fixture directories with junctions
after backup. Assert failure, retained backup, exact diagnostic path and unchanged
external fixture sentinel; no real user directory is redirected.

Actual Windows focused acceptance: seven direct-function Inno 6.7.3 cases passed:
locked generated EXE/DAT/MSG each reported its path and retained snapshot/marker;
installation-root junction, target-parent junction, backup-parent junction during
restore, and backup-parent junction during cleanup all refused mutation and
retained recovery data. Every outside fixture sentinel retained its SHA-256.
The independent fixture now validates the expanded installation directory before
any recovery write; early fixture setup failures are not counted as passes.
This supplements the full paired-EXE upgrade lifecycle, not a replacement for it.
