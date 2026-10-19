"""v0.7 generated installer/wiring contracts. No Windows execution or registry writes.

Actual transaction/fault and integration-backend tests live in Rust. These tests
exercise the real Python generator and guard its fixed native installer contract.
Diagnostic checks are source contracts, not an Inno compile or Windows execution.
The security fixtures model the bounded SID/ACE policy and are tied to source
constants/order below; they do not execute Win32 APIs or prove native races safe.
"""
from __future__ import annotations
import importlib.util
import json
import re
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("installer", ROOT / "scripts/build-installer.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)
MAIN = (ROOT / "installer/light-image-tool.iss").read_text("utf-8")
RECOVERY = (ROOT / "installer/recovery.iss").read_text("utf-8")
NATIVE = (ROOT / "src/updater/installer_native.rs").read_text("utf-8")


def routine(source, name):
    match = re.search(
        rf"^(?:function|procedure) {name}\b.*?(?=^(?:function|procedure) |\Z)",
        source,
        re.MULTILINE | re.DOTALL,
    )
    if not match:
        raise AssertionError(f"Missing routine: {name}")
    return match.group()


class ReconcileDiagnosticContracts(unittest.TestCase):
    def test_exec_launch_failure_reports_windows_code_and_description(self):
        command = routine(RECOVERY, "NativeCommand")
        launch = "Result := Exec(NativeHelper, Parameters, '', SW_HIDE, ewWaitUntilTerminated, Code);"
        self.assertEqual(command.count(launch), 1)
        self.assertNotIn("NativeLocksHeld and Exec", command)
        after_launch = command.split(launch, 1)[1]
        self.assertRegex(
            after_launch,
            r"if not Result then begin\s+"
            r"NativeLaunchError := 'Native transaction ' \+ Operation \+ "
            r"' could not launch \(Windows error ' \+\s+"
            r"IntToStr\(Code\) \+ ': ' \+ SysErrorMessage\(Code\) \+ '\)\.';\s+"
            r"Log\(NativeLaunchError\);\s+end\s+else Log\(",
        )
        reconcile = routine(RECOVERY, "RecoveryReconcile")
        self.assertRegex(
            reconcile,
            r"if not NativeCommand\('reconcile', Code\) then\s+"
            r"Result := NativeLaunchError\s+else if Code <> 0 then",
        )

    def test_unheld_locks_are_not_reported_as_an_exec_failure(self):
        command = routine(RECOVERY, "NativeCommand")
        guard = command.split("if not NativeLocksHeld then begin", 1)[1].split("end;", 1)[0]
        self.assertIn("Result := False;", command.split("if not NativeLocksHeld", 1)[0])
        self.assertIn("was not launched because the required locks are not held.", guard)
        self.assertIn("Log(NativeLaunchError); Exit;", guard)
        self.assertNotIn("SysErrorMessage", guard)
        self.assertLess(command.index("if not NativeLocksHeld"), command.index("Result := Exec("))

    def test_helper_nonzero_exit_is_not_a_windows_launch_error(self):
        reconcile = routine(RECOVERY, "RecoveryReconcile")
        exit_branch = reconcile.split("else if Code <> 0 then", 1)[1].split(";", 1)[0]
        self.assertEqual(exit_branch.strip(), "Result := NativeReconcileExitError(Code)")
        message = routine(RECOVERY, "NativeReconcileExitError")
        self.assertIn(
            "Result := 'Native transaction reconcile failed with helper exit code ' + IntToStr(Code) + '.';",
            message,
        )
        self.assertNotIn("SysErrorMessage", message)
        command = routine(RECOVERY, "NativeCommand")
        self.assertIn("else Log('Native transaction ' + Operation + ' returned ' + IntToStr(Code));", command)

    def test_only_known_helper_exit_codes_have_specific_security_diagnostics(self):
        message = routine(RECOVERY, "NativeSecurityExplanation")
        cases = re.findall(r"^\s*(\d+): Result := '((?:[^']|'')*)';$", message, re.MULTILINE)
        self.assertEqual([code for code, _ in cases], ["41", "42"])
        self.assertIn("not owned by the required Windows principal", cases[0][1])
        self.assertIn("review its ownership before retrying", cases[0][1])
        self.assertIn("unsafe or unsupported write permissions", cases[1][1])
        self.assertIn("review its write permissions before retrying", cases[1][1])
        for _, text in cases:
            self.assertIn("protected product path (installation, updater-state or Start Menu)", text)
            self.assertIn("do not bypass", text)
        # Exit 1 and every unknown exit retain the operation and numeric code,
        # without asserting an ownership, permissions or prior-health cause.
        self.assertNotIn("else", message)
        self.assertEqual(message.count("Result :="), 3)
        self.assertIn("Result := Result + NativeSecurityExplanation(Code);", routine(RECOVERY, "NativeReconcileExitError"))

    def test_windows_errors_41_and_42_cannot_enter_helper_diagnostic_mapping(self):
        reconcile = routine(RECOVERY, "RecoveryReconcile")
        dispatch = re.search(
            r"if not NativeCommand\('reconcile', Code\) then\s+"
            r"(Result := NativeLaunchError)\s+"
            r"else if Code <> 0 then\s+(Result := NativeReconcileExitError\(Code\));",
            reconcile,
        )
        self.assertIsNotNone(dispatch)
        self.assertEqual(reconcile.count("NativeReconcileExitError(Code)"), 1)
        command = routine(RECOVERY, "NativeCommand")
        self.assertNotIn("NativeReconcileExitError", command)
        self.assertNotRegex(command, r"Code\s*(?:=|<>)\s*(?:41|42)\b")
        # The exact source branch above covers both launch errors even when
        # their numbers collide with a documented helper exit code.
        self.assertEqual(
            dispatch.group(1), "Result := NativeLaunchError"
        )

    def test_success_keeps_empty_reconcile_result_and_clears_old_launch_error(self):
        command = routine(RECOVERY, "NativeCommand")
        prefix = command.split("if not NativeLocksHeld", 1)[0]
        self.assertIn("NativeLaunchError := '';", prefix)
        self.assertIn("Code := 0;", prefix)
        reconcile = routine(RECOVERY, "RecoveryReconcile")
        self.assertRegex(reconcile, r"begin\s+Result := '';\s+if RecoveryReady then Exit;")
        # Successful launch with exit zero has no assignment before this guard.
        tail = reconcile.split("else if Code <> 0 then", 1)[1]
        self.assertEqual(tail.split("if Result <> '' then begin", 1)[0].count("Result :="), 1)
        self.assertRegex(
            tail,
            r"if Result <> '' then begin\s+Result := Result \+ '[^']+' \+ NativeState \+ '\.';\s+"
            r"Log\(Result\);\s+end;\s+except\s+Result := GetExceptionMessage;",
        )

    def test_diagnostics_preserve_logs_without_reading_stale_helper_error(self):
        reconcile = routine(RECOVERY, "RecoveryReconcile")
        self.assertIn("Preserve the Setup log, plus any helper logs, journals and snapshots in ", reconcile)
        self.assertIn("Log(Result);", reconcile)
        self.assertIn("Result := GetExceptionMessage;", reconcile)
        for code in (reconcile, routine(RECOVERY, "NativeCommand"), routine(RECOVERY, "NativeReconcileExitError")):
            for forbidden in ("last-installer-error.txt", "LoadStringFromFile", "LoadStringsFromFile", "SaveStringToFile", "DeleteFile", "DelTree", "SetNamedSecurityInfo", "SetFileSecurity", "icacls", "takeown"):
                self.assertNotIn(forbidden, code)
        self.assertNotIn("An earlier installation needs health confirmation or repair", RECOVERY)


def security_policy_model(owner, aces, strict):
    """Synthetic SID/ACE fixture model; not a Win32 descriptor parser."""
    trusted = {"user", "system", "admins"}
    if not strict:
        trusted.add("trusted_installer")
    if owner not in ({"user"} if strict else trusted):
        return 41
    if aces is None:
        return 42
    mask_name = "NativeProductWriteMask" if strict else "NativeAncestorWriteMask"
    mask = int(re.search(rf"{mask_name} = \$([0-9A-F]+);", RECOVERY)[1], 16)
    for kind, flags, rights, sid in aces:
        if kind not in (0, 1):
            return 42
        if not strict and flags & 8:
            continue
        if kind == 1:
            continue
        if rights & mask and sid not in trusted:
            return 42
    return 0


class PrivatePathContracts(unittest.TestCase):
    def test_all_existing_objects_are_audited_before_first_creation(self):
        acquire = routine(RECOVERY, "AcquireNativeLocks")
        ordered = [
            "InitializeNativeSecurity",
            "PinNativeAncestors(NativeState)",
            "PinNativeAncestors(RemoveBackslashUnlessRoot(ExpandConstant('{app}')))",
            "PinExistingNativeLock(NativeState + '\\maintenance.lock')",
            "PinExistingNativeLock(NativeState + '\\sessions.lock')",
            "EnsureNativeProductDirectory(ExpandConstant('{localappdata}\\LightImageTool'), AllowCreate)",
            "EnsureNativeProductDirectory(NativeState, AllowCreate)",
            "TakeNativeLock(NativeState + '\\maintenance.lock', NativeMaintenance, AllowCreate)",
            "TakeNativeLock(NativeState + '\\sessions.lock', NativeSessions, AllowCreate)",
            "NativeLocksHeld := True",
        ]
        self.assertEqual([acquire.index(step) for step in ordered], sorted(acquire.index(step) for step in ordered))
        self.assertNotIn("ForceDirectories", RECOVERY)
        self.assertIn("if not Result then begin\n      Log(NativeAcquireError);\n      ReleaseNativeLocks;", acquire)
        for name in ("PinNativeAncestors", "PinExistingNativeLock"):
            code = routine(RECOVERY, name)
            self.assertNotIn("NativeCreateDirectory", code)
            self.assertNotIn("NativeCreateSecureFile", code)
            self.assertIn("$20081, 3, 0, 3,", code)  # read-only, no DELETE sharing, OPEN_EXISTING

    def test_pointer_abi_gate_precedes_first_native_identity_call(self):
        code = routine(RECOVERY, "InitializeNativeSecurity")
        self.assertNotIn("NativeUInt", RECOVERY)  # Not registered by the pinned Inno 6.7.3 compiler.
        self.assertIn("NativeNewDescriptor, NativeNewAcl: UINT_PTR;", RECOVERY)
        for variable, size in (("NativeUserSid", 4), ("NativeNewAttributes", 12), ("FileInfo", 52), ("AclInfo", 12), ("Ace", 8), ("Header", 4)):
            check = f"SizeOf({variable}) <> {size}"
            self.assertIn(check, code)
            self.assertLess(code.index(check), code.index("NativeOpenProcessToken"))
        self.assertIn("Needed > 65536", code)
        self.assertLess(code.index("Needed > 65536"), code.index("NativeLocalAlloc"))
        self.assertIn("NativeGetTokenInformation(Token, 1,", code)
        self.assertNotIn("TokenOwner", code)

    def test_descriptor_sid_and_ace_reads_are_bounded_before_copy(self):
        code = routine(RECOVERY, "NativeVerifyHandleSecurity")
        self.assertIn("NativeGetSecurityInfo(Handle, 1, $5", code)
        self.assertNotIn("GetNamedSecurityInfo", code)
        self.assertIn("DescriptorSize > 65536", code)
        self.assertIn("Acl.AceCount > 8191", code)
        self.assertIn("NativeBoundedSid(Owner, Descriptor, DescriptorSize)", code)
        self.assertIn("NativeRangeContains(Descriptor, DescriptorSize, Dacl, Acl.BytesInUse)", code)
        self.assertLess(code.index("NativeRangeContains(Dacl, Acl.BytesInUse, AceAddress, 4)"), code.index("NativeCopyAceHeader"))
        self.assertLess(code.index("if (Header.AceType <> 0) and (Header.AceType <> 1)"), code.index("NativeCopyAcePrefix"))
        self.assertLess(code.index("if (Header.AceType <> 0) and (Header.AceType <> 1)"), code.index("if not StrictProduct and ((Header.Flags and $8)"))
        self.assertLess(code.index("if Header.Size < 16"), code.index("NativeCopyAcePrefix"))
        self.assertIn("NativeBoundedSid(Sid, AceAddress, Header.Size)", code)
        self.assertIn("finally\n    if Descriptor <> 0 then NativeLocalFree(Descriptor);", code)
        self.assertIn("Base > High(Base) - Length", routine(RECOVERY, "NativeRangeContains"))

    def test_ancestor_and_strict_product_policy_match_bounded_fixture_rules(self):
        self.assertIn("NativeProductWriteMask = $500D0156;", RECOVERY)
        self.assertIn("NativeAncestorWriteMask = $500D0150;", RECOVERY)
        code = routine(RECOVERY, "NativeVerifyHandleSecurity")
        self.assertIn("if NativeEqualSid(Owner, NativeUserSid) = 0", code)
        self.assertIn("else if not NativeTrustedSid(Owner, False)", code)
        self.assertIn("if not StrictProduct and ((Header.Flags and $8) <> 0) then Continue;", code)
        self.assertIn("if Header.AceType = 1 then Continue;", code)
        self.assertIn("if (Header.AceType <> 0) and (Header.AceType <> 1) then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;", code)
        trusted = routine(RECOVERY, "NativeTrustedSid")
        for sid in ("NativeUserSid", "NativeSystemSid", "NativeAdminsSid"):
            self.assertIn(sid, trusted)
        self.assertIn("if not StrictProduct and not Result then Result := NativeEqualSid(Sid, NativeTrustedInstallerSid)", trusted)
        strict_paths = routine(RECOVERY, "NativeStrictProductPath")
        self.assertIn("Path = NativeState", strict_paths)
        self.assertIn("{localappdata}\\LightImageTool", strict_paths)
        self.assertIn("{app}", strict_paths)

    def test_path_syntax_is_rejected_before_pinning_or_writing(self):
        pin = routine(RECOVERY, "PinNativeAncestors")
        self.assertLess(pin.index("NativeCanonicalLocalPath(Path)"), pin.index("CreateFileW("))
        syntax = routine(RECOVERY, "NativeCanonicalLocalPath")
        for token in ("Length(Path) > 32760", "Path[2] <> ':'", "Path[3] <> '\\'", "Components > 128", "Part = '.'", "Part = '..'", "Length(Part) > 255", "Ord(Part[I]) < 32", "'<>:\"/|?*'", "Stem = 'CON'", "Stem = 'PRN'", "Stem = 'AUX'", "Stem = 'NUL'", "'COM'", "'LPT'"):
            self.assertIn(token, syntax)
        self.assertNotIn("ExpandFileName", syntax)
        self.assertNotIn("Lowercase(Path)", syntax)
        self.assertNotIn("Uppercase(Path)", syntax)

    def test_creation_has_explicit_token_user_owner_and_protected_private_dacl(self):
        code = routine(RECOVERY, "InitializeNativeSecurity")
        self.assertIn("NativeSetDescriptorOwner(NativeNewDescriptor, NativeUserSid, 0)", code)
        for sid in ("NativeUserSid", "NativeSystemSid", "NativeAdminsSid"):
            self.assertIn(f"NativeAddAllowedAce(NativeNewAcl, 2, 3, $1F01FF, {sid})", code)
        self.assertEqual(code.count("NativeAddAllowedAce("), 3)
        self.assertIn("NativeSetDescriptorControl(NativeNewDescriptor, $1000, $1000)", code)
        self.assertIn("NativeNewAttributes.InheritHandle := 0", code)
        for forbidden in ("SetFileSecurity", "SetNamedSecurityInfo", "SetSecurityInfo@", "icacls", "takeown", "powershell"):
            self.assertNotIn(forbidden, RECOVERY)

    def test_new_objects_require_protected_dacl_readback_without_repairing_existing(self):
        security = routine(RECOVERY, "NativeVerifyHandleSecurity")
        self.assertIn("if RequireProtected and ((Control and $1000) = 0)", security)
        directory = routine(RECOVERY, "EnsureNativeProductDirectory")
        self.assertIn("Created := False;", directory)
        self.assertIn("Created := NativeCreateDirectory(Path, NativeNewAttributes) <> 0;", directory)
        self.assertIn("NativeVerifyHandleSecurity(Handle, Path, True, Created)", directory)
        lock = routine(RECOVERY, "TakeNativeLock")
        self.assertIn("Code := DLLGetLastError;\n  Created := AllowCreate and (Code = 0);", lock)
        self.assertIn("NativeVerifyHandleSecurity(Handle, Path, True, Created)", lock)

    def test_lifetime_pins_request_data_access_to_enforce_deny_delete_sharing(self):
        for name in ("PinNativeAncestors", "PinExistingNativeLock", "EnsureNativeProductDirectory"):
            code = routine(RECOVERY, name)
            calls = re.findall(r"CreateFileW\(Path, (\$[0-9A-F]+), (\d+), 0, 3,", code)
            self.assertTrue(calls, name)
            for access, sharing in calls:
                self.assertEqual(int(access[1:], 16), 0x20081)
                self.assertTrue(int(access[1:], 16) & 1)  # FILE_LIST_DIRECTORY / FILE_READ_DATA
                # Directory pins reject writers; empty lock leaf pins retain
                # READ|WRITE sharing for byte-range lock contention semantics.
                self.assertEqual(int(sharing), 3 if name == "PinExistingNativeLock" else 1)

    def test_raced_creation_reopens_nofollow_and_audits_before_use(self):
        code = routine(RECOVERY, "EnsureNativeProductDirectory")
        self.assertIn("or not AllowCreate", code)
        self.assertIn("NativeCreateDirectory(Path, NativeNewAttributes)", code)
        self.assertIn("if Code <> 183 then", code)
        create = code.index("NativeCreateDirectory")
        reopen = code.index("Handle := CreateFileW", create)
        audit = code.index("NativeVerifyHandleSecurity(Handle, Path, True, Created)", reopen)
        self.assertLess(create, reopen)
        self.assertLess(reopen, audit)
        self.assertLess(audit, code.index("NativeKeepPin"))
        self.assertEqual(code.count("$02200000"), 2)

    def test_lock_is_nofollow_empty_single_link_and_checked_before_lockfile(self):
        code = routine(RECOVERY, "TakeNativeLock")
        self.assertIn("NativeCreateSecureFile(Path, $C0000000, 3, NativeNewAttributes, Creation, $00200080, 0)", code)
        self.assertIn("Creation := 3;\n  if AllowCreate then Creation := 4;", code)
        self.assertLess(code.index("NativeVerifyHandleType(Handle, Path, False)"), code.index("NativeLockFile("))
        self.assertLess(code.index("NativeVerifyHandleSecurity(Handle, Path, True, Created)"), code.index("NativeLockFile("))
        self.assertIn("CloseHandle(Handle); Handle := 0", code)
        metadata = routine(RECOVERY, "NativeVerifyHandleType")
        for check in ("NativeGetFileType(Handle) <> 1", "Info.Attributes and $400", "Info.Attributes and $10", "Info.Links = 1", "Info.SizeHigh = 0", "Info.SizeLow = 0"):
            self.assertIn(check, metadata)
        self.assertNotIn("MigrationRegularPath", code)

    def test_only_lock_violation_is_reported_as_contention(self):
        code = routine(RECOVERY, "TakeNativeLock")
        self.assertRegex(
            code,
            r"if not Result then begin\s+Code := DLLGetLastError;\s+"
            r"if Code = 33 then NativeAcquireError := '[^']+'\s+"
            r"else Result := NativeWindowsRefusal\('Cannot lock \"' \+ Path \+ '\"', Code\);",
        )

    def test_uninstall_uses_same_guard_before_reading_launcher_stamp(self):
        uninstall = routine(RECOVERY, "NativeUninstallPreflight")
        self.assertLess(uninstall.index("AcquireNativeLocks(False)"), uninstall.index("VerifiedInstalledLauncher"))
        self.assertLess(uninstall.index("VerifiedInstalledLauncher"), uninstall.index("CopyFile(InstalledExe"))
        self.assertIn("if NativeAcquireError <> '' then SuppressibleMsgBox(NativeAcquireError", routine(MAIN, "InitializeUninstall"))
        for name in ("RecoveryReconcile", "RecoveryBegin"):
            self.assertIn("if not AcquireNativeLocks(True) then begin\n    Result := NativeAcquireError; Exit;", routine(RECOVERY, name))

    def test_uninstall_stamp_and_launcher_leaves_are_private_bounded_and_immutable(self):
        pin = routine(RECOVERY, "NativePinReadFile")
        self.assertIn("CreateFileW(Path, $20081, 1, 0, 3, $00200080, 0)", pin)
        self.assertIn("NativeVerifyHandleSecurity(Handle, Path, True, False)", pin)
        self.assertEqual(pin.count("NativeVerifyReadFileType(Handle, Path, Maximum)"), 2)
        self.assertIn("CloseHandle(Handle); Handle := 0", pin)
        self.assertNotIn("NativeKeepPin", pin)
        metadata = routine(RECOVERY, "NativeVerifyReadFileType")
        for check in ("NativeGetFileType(Handle) <> 1", "Info.Attributes and $410", "Info.Links = 1", "Info.SizeHigh = 0", "Info.SizeLow > 0", "Info.SizeLow <= Maximum"):
            self.assertIn(check, metadata)
        self.assertIn("NativeLauncherMaxBytes = 536870912;", RECOVERY)

    def test_uninstall_reads_only_after_leaf_pins_and_checks_the_copied_identity(self):
        verified = routine(RECOVERY, "VerifiedInstalledLauncher")
        self.assertLess(verified.index("if not NativeLocksHeld"), verified.index("NativePinReadFile"))
        self.assertLess(verified.index("NativePinReadFile(Path, 256, NativeStampPin)"), verified.index("LoadStringsFromFile"))
        self.assertLess(verified.index("NativePinReadFile(InstalledExe, NativeLauncherMaxBytes, NativeLauncherPin)"), verified.index("GetSHA256OfFile(InstalledExe)"))
        self.assertIn("NativeVerifiedLauncherHash := Hash", verified)
        self.assertIn("finally\n    if not Result then ReleaseNativeBootstrapPins;", verified)
        uninstall = routine(RECOVERY, "NativeUninstallPreflight")
        ordered = ["VerifiedInstalledLauncher", "CopyFile(InstalledExe", "GetSHA256OfFile(NativeHelper) <> NativeVerifiedLauncherHash", "ReleaseNativeBootstrapPins;", "NativeCommand('uninstall-check'"]
        self.assertEqual([uninstall.index(token) for token in ordered], sorted(uninstall.index(token) for token in ordered))
        self.assertIn("finally\n    ReleaseNativeBootstrapPins;", uninstall)
        release = routine(RECOVERY, "ReleaseNativeBootstrapPins")
        for handle in ("NativeStampPin", "NativeLauncherPin"):
            self.assertIn(f"CloseHandle({handle}); {handle} := 0", release)
        self.assertNotIn("NativePathPins", release)
        self.assertNotIn("NativeMaintenance", release)
        self.assertNotIn("NativeSessions", release)

    def test_protected_read_leaf_fixture_refuses_aliases_and_foreign_writers(self):
        def admitted(attributes=0x80, links=1, size=100, maximum=256, owner="user", aces=None):
            if aces is None:
                aces = [(0, 0, 0x1F01FF, "user")]
            return not attributes & 0x410 and links == 1 and 0 < size <= maximum and security_policy_model(owner, aces, True) == 0
        self.assertTrue(admitted())
        for changes in ({"attributes": 0x400}, {"attributes": 0x10}, {"links": 2}, {"links": 0}, {"size": 0}, {"size": 257}, {"owner": "admins"}, {"aces": [(0, 0, 2, "foreign")]}):
            self.assertFalse(admitted(**changes), changes)
        self.assertTrue(admitted(size=536870912, maximum=536870912))
        self.assertFalse(admitted(size=536870913, maximum=536870912))

    def test_existing_unsafe_product_fixtures_are_refused(self):
        safe = [(0, 0, 0x1F01FF, "user"), (0, 0, 0x1F01FF, "system"), (0, 0, 0x1F01FF, "admins")]
        self.assertEqual(security_policy_model("user", safe, True), 0)
        for owner in ("admins", "system", "trusted_installer", "foreign"):
            self.assertEqual(security_policy_model(owner, safe, True), 41)
        for bad in (None, [(0, 0, 2, "foreign")], [(0, 8, 2, "foreign")], [(5, 0, 0, "user")]):
            self.assertEqual(security_policy_model("user", bad, True), 42)
        self.assertEqual(security_policy_model("user", [(0, 0, 0x120089, "foreign")], True), 0)

    def test_os_ancestor_fixtures_allow_trusted_owners_without_foreign_destruction(self):
        for owner in ("user", "system", "admins", "trusted_installer"):
            self.assertEqual(security_policy_model(owner, [(0, 0, 0x1F01FF, owner)], False), 0)
        # Create-only rights on C:\\ and inherited-only creator grants do not
        # authorize replacement or ACL changes to the pinned existing parent.
        self.assertEqual(security_policy_model("admins", [(0, 0, 6, "foreign"), (0, 8, 0x1F01FF, "foreign")], False), 0)
        for dangerous in (0x10, 0x40, 0x100, 0x10000, 0x40000, 0x80000, 0x10000000, 0x40000000):
            self.assertEqual(security_policy_model("admins", [(0, 0, dangerous, "foreign")], False), 42)
        self.assertEqual(security_policy_model("foreign", [], False), 41)
        self.assertEqual(security_policy_model("admins", [(9, 0, 0, "admins")], False), 42)
        self.assertEqual(security_policy_model("admins", [(9, 8, 0, "admins")], False), 42)
        self.assertEqual(security_policy_model("admins", [(1, 0, 0x1F01FF, "foreign")], False), 0)


class NativeInstallerContracts(unittest.TestCase):
    def test_stable_launcher_is_never_a_normal_inno_copy(self):
        with tempfile.TemporaryDirectory(prefix="gloamere-installer-fixture-") as temp:
            files, _ = build.generate_includes(["gloamere-image.exe", "runtime/gloamere-image-ui.exe", "runtime/gloamere-image-worker.exe", "MANIFEST.json"], Path(temp))
            text = files.read_text("utf-8")
            self.assertNotIn('Source: "{#PayloadDir}\\gloamere-image.exe"', text)
            self.assertIn('DestDir: "{app}\\runtime"', text)
            self.assertNotIn("restartreplace", text)
        self.assertIn('DestName: "gloamere-image-recovery.exe"; Flags: dontcopy', MAIN)
        self.assertIn("if self.journal.bootstrap", NATIVE)
        self.assertIn("MOVEFILE_WRITE_THROUGH", NATIVE)
        self.assertNotIn("MOVEFILE_DELAY_UNTIL_REBOOT", NATIVE)

    def test_plan_is_generated_from_exact_payload_identity(self):
        with tempfile.TemporaryDirectory(prefix="gloamere-plan-fixture-") as temp:
            root=Path(temp); payload=root / "payload"; payload.mkdir()
            (payload / "MANIFEST.json").write_bytes(b"synthetic manifest bytes")
            manifest={"files":[{"path":"runtime/gloamere-image-ui.exe","size_bytes":8,"sha256":"a"*64}]}
            path=build.generate_transaction_plan(payload,manifest,root)
            plan=json.loads(path.read_text("utf-8"))
            self.assertEqual(plan["schema"],1)
            self.assertEqual(plan["product_id"],build.PRODUCT_ID)
            self.assertEqual(plan["launcher_protocol"],1)
            files={row["path"]:row for row in plan["files"]}
            self.assertEqual(files["MANIFEST.json"]["sha256"],build.sha256(payload / "MANIFEST.json"))
            self.assertEqual(files[build.OWNER_MARKER]["length"],len(build.PRODUCT_ID))
            self.assertNotIn("command",plan)
            self.assertNotIn("registry",plan)

    def test_setup_keeps_snapshots_until_ui_health(self):
        self.assertNotIn("RecoveryCleanup",RECOVERY)
        self.assertIn("if CurStep = ssPostInstall then begin",MAIN)
        self.assertIn("RecoveryInstalled;",MAIN)
        self.assertIn("if not NativePendingHealth",MAIN)
        self.assertIn("'pending-health'",RECOVERY)
        self.assertIn("Phase::PendingHealth",NATIVE)
        self.assertIn("Phase::CommittingHealth",NATIVE)
        self.assertIn("report.ui_event_loop",NATIVE)
        self.assertIn("report.built_in_decode",NATIVE)
        self.assertIn("report.settings_readable",NATIVE)

    def test_setup_uninstall_and_runtime_use_same_lock_names(self):
        for lock in ("maintenance.lock","sessions.lock"):
            self.assertIn(lock,RECOVERY)
            self.assertIn(lock,NATIVE)
        self.assertIn("NativeUninstallPreflight",MAIN)
        uninstall=RECOVERY.split("function NativeUninstallPreflight",1)[1]
        self.assertLess(uninstall.index("VerifiedInstalledLauncher"),uninstall.index("CopyFile(InstalledExe"))
        self.assertIn("launcher.sha256",RECOVERY)
        self.assertIn("LAUNCHER_STAMP",NATIVE)
        self.assertIn("DeinitializeUninstall",RECOVERY)
        self.assertIn("runtime_admission",NATIVE)
        self.assertIn("LockFile@kernel32.dll",RECOVERY)
        self.assertIn("NativeGetCurrentProcessId",RECOVERY)
        self.assertIn("Caller is not the declared Setup parent",NATIVE)

    def test_no_forceclose_reboot_or_interrupt_commit(self):
        for setting in ("PrivilegesRequired=lowest","CloseApplications=no","RestartApplications=no","AlwaysRestart=no","AllowCancelDuringInstall=no","UsePreviousAppDir=yes","UsePreviousTasks=yes"):
            self.assertIn(setting,MAIN)
        self.assertNotIn("restartreplace",MAIN)
        self.assertNotIn("TerminateProcess",NATIVE)
        self.assertIn("code == 3010",NATIVE)
        self.assertIn("Phase::RepairRequired",NATIVE)
        self.assertNotIn("/TASKS=",RECOVERY)

    def test_recovery_does_not_delete_unknown_trees_or_user_settings(self):
        self.assertNotIn("remove_dir_all",NATIVE)
        self.assertNotIn("DelTree(",RECOVERY)
        self.assertIn("Unknown new file bytes",NATIVE)
        self.assertIn("Foreign, changed or missing owned file",NATIVE)
        self.assertIn("Restored file readback mismatch",NATIVE)
        self.assertIn("snapshot_tampering_prevents_begin",NATIVE)
        self.assertIn("injected_disk_full_or_power_interruption",NATIVE)

    def test_parent_handle_ready_handshake_precedes_parent_exit(self):
        self.assertIn("CreateEventW",NATIVE)
        self.assertIn("SetEvent",NATIVE)
        self.assertIn("WaitForSingleObject(handle",NATIVE)
        self.assertIn("RecoverySpawned",NATIVE)
        self.assertIn("Parent PID was reused",NATIVE)

if __name__ == "__main__":
    unittest.main(verbosity=2)
