# Actual Inno security/diagnostic ABI runtime fixture, before code

Compile the exact candidate recovery-types.iss and recovery.iss into a distinct
nonshipping fixture executable: no Files, Registry, Icons or Run sections;
Uninstallable=no, CreateAppDir=no, InitializeSetup always returns false before
installation. Different AppId/name. Never call AcquireNativeLocks, Recovery*,
NativeUninstallPreflight or any real helper/KnownFolder product path. Test only
InitializeNativeSecurity, PinNativeAncestors on approved OS temp ancestry, new
EnsureNativeProductDirectory, new/reused/pinned empty lock files and NativeCommand
with a task-built harmless stub that only exits41/42/1. Missing helper tests an
actual Exec(false) launch failure. Preserve fixed numeric distinction and logs.

Fixture root is a fresh C:/Windows/Temp/gloamere-inno-security-UUID; execution is
approved/elevated for new task-private temporary objects only. New root/locks use
the ACTUAL candidate security-attributes/Win32 bindings, proving native pointer,
record, token, descriptor and lock calls beyond compiler acceptance. Only the
Inno engine may create/clean its own temporary directory; no existing ACL setter,
privilege changes, actual app install/uninstall, scoped registry or reboot action.
Python records existing OS-parent descriptors/identities before and after, new
root/file owner/DACL and actual byte sizes. Existing managed app files/registry
remain checked separately. A default SaveStringToFile synthetic leaf is a negative
ownership probe, not claimed to be the Inno Files engine's behavior.

This does not establish full Setup lifecycle, actual helper transfer immutability,
or ordinary/split-token acceptance. Treat unsupported ABI/refusals as FAIL and
retain logs; never change the ABI guard just to pass. Fixture success is its
explicit QA result, not InitializeSetup's expected cancellation exit status.
# Fixture timing correction

Execution04's read-only ABI probes located the remaining harness error:
CreateAppDir=no makes Inno {app} equal C:\Windows, incorrectly classifying the
Windows ancestor as a strict product root. Set CreateAppDir=yes and
DefaultDirName={param:FixtureRoot} solely to initialize {app} to the fresh bounded
synthetic root. The unconditional PrepareToInstall error still prevents engine
directory creation/installation. Production recovery.iss remains byte-identical
to candidate input. Fixture functions alone explicitly create the private root.

Execution02 showed {app} is still unavailable at InitializeWizard. Execution03
uses PrepareToInstall, the exact event used by the product's AcquireNativeLocks.
Always return a nonempty error string and NeedsRestart=False before installation.
Preserve both timing failures. Only compilation was confirmed for01/02, not the
actual security primitive matrix; neither was a product-guard or ABI failure.

Execution01 stopped after2 checks because NativeStrictProductPath expands {app},
which Inno has not initialized during InitializeSetup. This is a harness timing
error. Run the same included production functions from InitializeWizard instead,
then unconditionally Abort before installation. There remain no files, registry,
icons or run sections and Uninstallable=no. Preserve execution01 and rebuild a
new runtime02 binary, distinct from any actual product installer.

