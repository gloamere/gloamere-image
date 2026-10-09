# gloamere-image branding and EXE-only migration design

Written before implementation, 2026-10-09. Version remains 0.4.0. This is a
source-level design and Windows acceptance plan, not a claim of completed Windows
runtime validation.

## User outcome and distribution

The public product name is `gloamere-image`; the main and worker executables are
`gloamere-image.exe` and `gloamere-image-worker.exe`. Setup, Installed Apps,
shortcuts, candidate-handler labels and context-menu labels use the new name.
`assets/gloamere-image.ico` supplies setup/uninstall/shortcut/association icons;
its source SVG and raster derivatives live in the source tree.

A new installation and a manual update both run the same per-user x64 Setup EXE.
There is no portable ZIP as an end-user install/update path, no background updater,
network endpoint, domain purchase or signing purchase. An unsigned build remains
explicitly unsigned. Internal payload directories and source archives remain
useful developer/QA artifacts. The Windows command `powershell -NoProfile -File scripts/build-release.ps1 -Compiler "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"` must run code and
installer-contract gates, build the paired binaries, validate an exact-file/hash
manifest and compile the installer, then emit the Setup EXE, SHA-256 and build
report. Compiler/build tools are supplied by the developer, never silently fetched.

## Identities deliberately retained

Keep AppId/OwnerId `9CCB6F43-7782-4F69-9E91-67C6D144737B`, lowest privileges,
x64 install mode, `UsePreviousAppDir=yes`, the historical default directory
`{userpf}\\LightImageTool`, `LIGHT-IMAGE-INSTALL.txt`, the recovery directory,
`LightImageTool` Explorer verb, `Software\\LightImageTool` ownership root and
`LightImageTool.Image` ProgID. The old settings directory/schema stays with the
app implementation. These are compatibility identifiers rather than marketing
labels. Reusing them preserves the one uninstall entry and user-selected image
handler identity, avoiding a second installation or loss of preferences.

## Ownership and migration state machine

Before any writes, confirm the selected directory matches the existing uninstall
InstallLocation as exact REG_SZ text (case-insensitive path equivalence is allowed
only for directory identity), and its marker matches OwnerId. Refuse non-owned
nonempty directories and unavailable old/new main/worker files. No app is killed.

Explorer registrations have two exact profiles: the old label/path/icon and the
new label/path/icon. A profile matches only when every known value is REG_SZ with
exact text, the value count matches, and no unknown subkeys/values are present.
An old profile is accepted only in the same marker- and uninstall-owned directory.
Refuse foreign/partial/mixed registrations before writes even when the Explorer
task is unchecked, so migration cannot silently strand an old handler. The task
remains optional; unchecked removes only a fully matching owned old/new menu.

Candidate-handler registration is a complete old or new profile, including both
product trees and its one RegisteredApplications discovery value. An entirely
absent profile is the normal v0.3 upgrade/fresh-install case. The old v0.4 r2
candidate may be recognized conservatively even though it was not installed on
the user's actual machine. Mixed, partial, unknown, differently typed or duplicate
discovery registrations fail closed. The new profile preserves the old ProgID,
changes display labels/OwnerPath/icon/command, publishes `gloamere-image` and
removes the exactly matching old discovery value. Track writes and removal so
failure restores the original profile without overwriting concurrent foreign
changes. Shared registry parents are never pruned.

No extension default values, UserChoice, policies, HKLM or HKCR are changed. The
Windows Default Apps action stays a one-time postinstall unchecked action, never
a remembered task; older Windows use the generic settings page.

## Bounded obsolete cleanup and complete recovery

Recovery snapshots the union of new payload paths, the two fixed old executable
names, exact existing Inno uninstaller names, owned menu/candidate/uninstall
values, and the two fixed old/new Start Menu shortcut files. It never follows
paths listed by an installed manifest. Old executable deletion requires the owned
installation plus the fixed filename's recorded installed-manifest hash; changed
or missing evidence refuses migration instead of guessing. The manifest reader
accepts only the canonical bounded record format emitted by our packager and
extracts hashes for the two hard-coded names. It cannot nominate another path.

Only the fixed historical Start Menu shortcut
`{userprograms}\\Light Image Tool\\Light Image Tool.lnk` is obsolete. Inspect its
shortcut target and empty arguments before treating it as owned. An existing
fixed old shortcut whose target cannot be verified blocks upgrade and is preserved;
this includes disabled Windows Script Host/COM inspection. Unknown custom shortcut
locations are neither read nor modified. The new fixed shortcut path requires the existing marker if already present and also receives a collision
preflight and a snapshot if it exists. Shortcut backup files live in the owned
recovery directory, but destination paths come only from these two constants.
Never perform wildcard tree deletion or recursively delete a Start Menu folder;
remove the old parent only if empty. Check reparse ancestors before every backup,
restore and removal and retain recovery evidence on failure.

Perform bounded cleanup only after the verified snapshot and before any success
state. Every cleanup failure must make installation fail and trigger explicit
restoration. Recovery must restore exact old paired EXE hashes, old manifest,
uninstaller bytes, shortcut bytes and typed registration. Newly absent paths are
removed only from the bounded list. Preserve images/preferences/unknown files.
The inherited Inno uninstall log can contain the old shortcut path; uninstall therefore refuses if either fixed shortcut now points elsewhere, preserving it until the user renames or resolves it. Custom legacy Start Menu locations are not discovered or modified.
Concurrent replacement races, forced termination and power loss remain limitations,
not claimed atomic transaction guarantees.

## Windows QA release gate

1. Fresh Windows 10/11 install: one installed-app entry, new EXE pair and new
   shortcuts/icons; version 0.4.0; initial unchecked Explorer and Default Apps.
2. Upgrade actual v0.3 with its marker/manifest: existing install path/settings/
   user image retained, same AppId, new pair present, exact old pair gone, old owned
   shortcut replaced, no duplicate candidate or installed-app entries.
3. Repeat v0.3 with Explorer selected and unselected; all jpg/jpeg/png handlers
   target new EXE when selected; unchecked only removes owned known menu values.
4. Optional r2 fixture with full old candidate registration: exact old profile
   becomes one new discovery value and new display labels while ProgID stays stable.
5. Refuse before payload writes: REG_EXPAND_SZ with matching text; foreign owner;
   changed command/label/icon; extra tree/value; partial candidate; duplicate old/new
   discovery; moved install; changed legacy EXE hash; missing manifest evidence;
   locked old/new worker or main; reparse payload/shortcut/recovery ancestors.
6. Inject failure after both new EXE bytes were observed and after legacy cleanup;
   compare every old payload/uninstaller/manifest/shortcut hash and typed registry
   snapshot. Newly absent paths must again be absent. Test native cancellation too.
7. Preserve foreign concurrent menu/candidate registry values and report/retain recovery when
   restoration cannot safely overwrite them. Preserve shared registry parents.
8. Upgrade success then uninstall: remove own payload/registration/shortcuts only;
   retain user images/settings/foreign registrations. Refuse uninstall when a fixed
   historical/current shortcut has changed target, because the inherited Inno log
   could otherwise delete it. Rename/preserve the foreign shortcut before retrying.
9. Visual inspection: Explorer EXE icon, Alt-Tab/taskbar/titlebar, setup wizard,
   Installed Apps, Start Menu shortcut, right-click menu and Default Apps entry.
10. Reinstall same version and manual later-version upgrade using only the Setup
    EXE; repeat test with no network. No hidden ZIP/portable path is recommended.

Linux script tests cover manifest validation and source safety contracts. They do
not substitute for the Windows compile and lifecycle matrix above. A release report
must distinguish run/passed, failed and not-run stages.

## Official references consulted

- Inno same-application identity: https://jrsoftware.org/ishelp/topic_sameappnotes.htm
- Inno setup icon: https://jrsoftware.org/ishelp/topic_setup_setupiconfile.htm
- Inno uninstall icon: https://jrsoftware.org/ishelp/topic_setup_uninstalldisplayicon.htm
- Inno shortcut icon: https://jrsoftware.org/is6help/topic_iconssection.htm
- Inno installation ordering: https://jrsoftware.org/ishelp/topic_installorder.htm
- Microsoft default-app platform: https://learn.microsoft.com/en-us/windows/apps/develop/windows-integration/default-apps-platform

## Validation status in this cloud task

The 14 default-app contracts and 19 migration source/model tests pass on Linux.
Canonical manifest model tests exercise the legacy packager record shape, CRLF,
missing/duplicate hashes, wrong paths and invalid fields; they do not execute
Pascal Script. Windows Inno compilation and the complete lifecycle matrix above
remain mandatory before publishing the installer. The dedicated uninstall
namespace remains restored as a typed snapshot; concurrent edits to that namespace
are outside the menu/candidate compare-before-restore guarantee.

### Review hardening: shortcut aliases

Inno's standard shortcut creation also considers same-stem `.pif`, `.url` and
directory shortcuts. Preflight and the fatal install boundary refuse any such
new-brand collision. The new shortcut uses `uninsneveruninstall`; uninstall
removes it only after the target/arguments ownership check. Both historical and
current alias collisions block uninstall because older appended Inno logs can
contain their exact paths. No binary uninstall log is edited. Add these alias
fixtures to Windows QA, including a foreign URL/PIF sentinel with unchanged hash.
The dedicated uninstall registry key also refuses unexpected subkeys because the
bounded typed snapshot cannot restore subtrees. Backup hashes are checked before
copying bytes back during recovery.

References for this bounded hardening:
- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Setup.Install.pas#L1808-L1861
- https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Setup.UninstallLog.pas#L993-L1005

### Release-command implementation and checks

`build-release.ps1` accepts required `-Compiler` and optional `-BuildTarget`,
`-Output` (a fresh directory below `dist`), `-PythonCommand` and `-SourceCommit`.
It rejects the cloud-only `GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY` override and
uses one explicit MSVC target/target-directory for debug build/tests and release
build/tests. Each profile's complete targets are built before its tests. Silent
successful commands produce a valid empty log rather than passing null to the
PowerShell file API. The release directory receives only the unsigned Setup EXE,
its SHA256 and build reports; staging/evidence remain under the internal directory.

Additional Linux checks passed: 31 packaging contracts and 5 resource-parser
contracts. The offline collector/stager smoke used synthetic EXEs and the actual
product icon: 358 locked packages, 659 notice files plus three explicit historical
declaration fallbacks, zero unresolved gaps, and 671 validated staged files.
These are packaging checks, not real PE execution or Windows lifecycle proof.
