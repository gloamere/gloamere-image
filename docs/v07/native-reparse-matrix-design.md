# Native ancestor redirection matrix (2026-10-10)

Purpose: test the candidate's actual create-only ancestor exception, as well as
WRITE_EA/WRITE_ATTRIBUTES, without changing real profile, product, registry or ACLs.
An ordinary launcher must neither redirect its update namespace nor mistake an
ambient policy rejection for successful installation acceptance.

Before execution, create a fresh protected TokenUser/SYSTEM/Administrators-owned
fixture under the already readable C:\Windows\Temp. Create every test object anew
with an explicit protected descriptor. Never reset existing ACLs, adjust token
privileges, delete objects recursively, or redirect an existing directory.
Retain artifacts for inspection. Pin existing OS parents and compare OWNER+DACL
using GetSecurityInfo mask 5 and volume/file identity before and after.

Matrix: requested access exactly 0x10, 0x100, 0x110 and 0x2; empty directory or
one protected child; no pin, current FILE_LIST_DIRECTORY|READ_ATTRIBUTES|
READ_CONTROL pin with share3, or the same access with share1. Each case uses a
fresh directory. Acquire pin before attacker handle, record actual open errors,
then issue FSCTL_SET_REPARSE_POINT or EX to a fresh private sibling target.
Use a mount-point tag, not symlink privilege or a changed security policy.
Record raw buffer, IOCTL code, Win32 error, security, attributes and stable
identity. Resolve redirected content only inside the private fixture and only
after comparing no-follow handle attributes. No cleanup IOCTL is necessary.

The exact-access handle is a restricted-rights probe, not impersonation of an
untrusted principal. Record token privileges unchanged and do not infer full
multi-principal exploit proof from it. EX is a separate observation; unsupported
EX cannot invalidate a supported ordinary IOCTL positive control. New
CreateDirectory2W is probed for export availability on Win10, never assumed.

References: Microsoft MS-FSA FSCTL_SET_REPARSE_POINT permits FILE_WRITE_DATA
(0x2, alias FILE_ADD_FILE on directories) OR FILE_WRITE_ATTRIBUTES (0x100).
Nonempty directories are refused. Microsoft's CreateDirectory2W share-read
sample has minimum client Windows 11 24H2, so Win10 CreateFile pins require
actual testing. No mask exception or production pin change is authorized by
the design; first report matrix results and request independent review.

https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165
https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createdirectory2w

Follow-up after the 48-case result: test restricted handle opened BEFORE the pin
for each exact mask and share1/share3; share1 must refuse an already open0x2 writer
as well as new writers. Separately prove child creation still works while a
share1 parent pin is held. This tests a potential minimal mitigation, not a
production change or proof that0x100 can be admitted.
