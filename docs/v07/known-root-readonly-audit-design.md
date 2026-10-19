# OS known-root audit before namespace decisions

Read-only scope, 2026-10-12 UTC. Query actual SHGetKnownFolderPath results for
ProgramData, ProgramFiles, LocalAppData, LocalAppDataLow, Profile and Public,
plus existing system-drive root. For each existing root and every ancestor,
open no-follow read-security/read-attributes handles, query GetSecurityInfo
OWNER+DACL mask5, type/reparse flag and volume/file identity. Do not create,
set security, adjust privileges, rename or delete any object. Retain stable
repeated snapshots and report denials/missing paths explicitly.

Report the existing ancestor mask as policy evidence, not exploit proof or
permission to relax it. Effective standard-user child-directory creation is
checked with AccessCheck(FILE_ADD_SUBDIRECTORY) using an already available
linked standard token; duplicate only to an impersonation token for AccessCheck,
never impersonate to perform writes. If no linked token exists, report that
standard-user creation is NOT_VERIFIED instead of deriving it from Administrators
or sandbox-user grants. Also report the existing token's effective creation right.
AccessCheck is a read-only estimate, not proof of actual CreateDirectory success.

A root is only a prospective namespace candidate when owner/reparse/foreign
mutation policy and effective creator access admit it. No actual app location
is selected; namespace collision, lifecycle pin behavior, parent-child binding,
trusted OS cleanup and model/settings/TUF migration remain separate decisions.
ProgramData is not presumed safe solely because it is OS known. A failed host
policy never justifies unknown-SID trust or resetting user directory permissions.

## Actual read-only result

SHGetKnownFolderPath returned C:\ProgramData, C:\Program Files, the actual
Administrator Profile/Local/LocalLow, and C:\Users\Public. ProgramData has a
foreign effective mutation ACE and fails policy; all user-profile roots fail
on their0x110 ancestry. ProgramFiles, Public and C:\ passed the existing
descriptor/type policy, but this is NOT a qualified product namespace.
The elevated token has no linked standard token (GetTokenInformation error1312).
Only elevated-token AccessCheck allowed child creation. Actual standard-user
creation is NOT_VERIFIED; no directory was created, so neither usability nor
complete lifecycle admission is proved. Raw repeated mask5 security snapshots,
identities and separate mask7 AccessCheck data are in
v07-windows-qa/known-roots-readonly-20261012.json.

## Concrete decisions and migration costs

1. A clean controlled Windows10/11 environment with a real standard token is
   the smallest verification decision. It proves supported per-user KnownFolder
   deployment without changing this host; it does not silently fix this host.
2. A newly selected private app namespace under a prospective root requires
   explicit approval of the exact real location, user/admin ownership model,
   installer layout and registry/shortcut migration. ProgramFiles may require
   an elevated bootstrap; Public/C:\ have shared namespace/collision and OS
   policy concerns, and are not selected for production by this audit. State,
   downloads and model activation must all use qualified roots, not fall back
   to the rejected current LocalAppData or ProgramData.
3. An OS-managed install migration needs packaging identity/certificates and
   old-Inno migration review. No machine installation or certificate work is
   authorized now. If standard-user/private-root qualification fails, report
   clean-environment or separately approved managed migration rather than relax
   the unsafe profile rule.

Settings import treats old JSON as untrusted data: bounded parse and allowlisted
preferences only, no root/server/installer authorization imported. Models require
exact signed content/compatibility identity and a fresh self-test; copy bytes
does not transfer trust or consent. Preserve independent application/model
choices and caches as data only. TUF root remains compiled/authorized, role
versions and quarantine must not silently reset during migration; a root that
cannot securely preserve release floors requires a designed authenticated
continuity mechanism or refuses migration. User images stay at existing paths;
new file associations/shortcuts and uninstall registration are an explicitly
approved transactional migration, not a side effect of read-only audit.
