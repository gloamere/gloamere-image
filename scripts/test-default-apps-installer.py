"""Linux-safe source contracts, NOT an Inno compile or Windows lifecycle test."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAIN = (ROOT / "installer/light-image-tool.iss").read_text("utf-8")
DEFAULTS = (ROOT / "installer/default-apps.iss").read_text("utf-8")
MENUS = (ROOT / "installer/menus.iss").read_text("utf-8")
RECOVERY = (ROOT / "installer/recovery.iss").read_text("utf-8")
REGISTRY_PATH = ROOT / "installer/registry-values.iss"
REGISTRY = REGISTRY_PATH.read_text("utf-8") if REGISTRY_PATH.exists() else ""


def routine(source, name):
    match = re.search(
        rf"^(?:function|procedure) {name}\b.*?(?=^(?:function|procedure) |\Z)",
        source,
        re.MULTILINE | re.DOTALL,
    )
    if not match:
        raise AssertionError(f"Missing routine: {name}")
    return match.group()


class DefaultAppsInstallerContracts(unittest.TestCase):
    def test_opt_in_postinstall_not_persisted_task(self):
        section = MAIN.split("[Run]\n", 1)[1].split("[", 1)[0]
        entries = [line for line in section.splitlines() if line.startswith("Filename:")]
        self.assertEqual(len(entries), 1)
        self.assertIn('Filename: "{code:DefaultAppsSettingsUri}"', entries[0])
        self.assertEqual(
            set(entries[0].split("Flags: ", 1)[1].split()),
            {"postinstall", "unchecked", "shellexec", "skipifsilent", "nowait"},
        )
        tasks = MAIN.split("[Tasks]\n", 1)[1].split("[", 1)[0]
        self.assertEqual(tasks.count("Name:"), 1)
        self.assertIn('Name: "explorer"', tasks)
        self.assertIn("打开 Windows 默认应用设置", MAIN)

    def test_uri_has_conservative_old_windows_fallback(self):
        code = routine(DEFAULTS, "DefaultAppsSettingsUri")
        self.assertIn("Result := 'ms-settings:defaultapps';", code)
        self.assertIn("Version.Major = 10", code)
        self.assertIn("Version.Minor = 0", code)
        self.assertIn("Version.ProductType = VER_NT_WORKSTATION", code)
        self.assertIn("Version.Build >= 22631", code)
        self.assertIn("?registeredAppUser=gloamere-image", code)

    def test_handler_claims_only_jpeg_and_png(self):
        code = routine(DEFAULTS, "PopulateDefaultAppProfile")
        claims = re.findall(
            r"AddDefaultAppValue\(Values, Capabilities \+ '\\FileAssociations', '([^']+)', '([^']+)'\);",
            code,
        )
        self.assertEqual(
            claims,
            [(ext, "LightImageTool.Image") for ext in (".jpg", ".jpeg", ".png")],
        )
        self.assertEqual(code.count("AddDefaultAppValue(Values,"), 14)
        self.assertNotIn("UrlAssociations", code)
        self.assertNotIn("MimeAssociations", code)
        self.assertIn("'ApplicationName', LabelName", code)
        self.assertIn("'ApplicationDescription'", code)
        self.assertIn("'ApplicationIcon'", code)
        self.assertIn("ProgId + '\\shell\\open\\command', '', MenuCommand(Path)", code)
        last_value = [line for line in code.splitlines() if "AddDefaultAppValue(" in line][-1]
        self.assertIn("'Software\\RegisteredApplications', LabelName, Capabilities", last_value)

    def test_registration_is_hkcu_only_without_default_selection(self):
        code = "\n".join(line for line in DEFAULTS.splitlines() if not line.strip().startswith("//"))
        for forbidden in ("UserChoice", "HKLM", "HKCR", "SetAppAsDefault", "RegDeleteKeyIncludingSubkeys"):
            self.assertNotIn(forbidden, code)
        self.assertNotRegex(code, r"Software\\Classes\\\.")
        for call in re.findall(r"\bReg\w+\(([^;\n]*)", code):
            self.assertTrue(call.startswith("HKCU,"), call)
        self.assertIn("PrivilegesRequired=lowest", MAIN)
        self.assertIn("ChangesAssociations=yes", MAIN)
        self.assertNotRegex(MAIN, r"Flags:\s*uninsdelete")

    def test_preflight_precedes_snapshot_and_writes(self):
        code = routine(MAIN, "PrepareToInstall")
        self.assertLess(code.index("if not DefaultAppsSafe"), code.index("Result := RecoveryBegin;"))
        install = routine(DEFAULTS, "InstallDefaultApps")
        self.assertLess(install.index("if not DefaultAppsSafe"), install.index("RegWriteStringValue"))
        steps = routine(MAIN, "CurStepChanged")
        self.assertIn("if CurStep = ssInstall then begin\n    RecoveryStarted := True;", steps)
        self.assertLess(steps.index("RecoveryStarted := True;"), steps.index("InstallDefaultApps;"))
        self.assertLess(steps.index("InstallDefaultApps;"), steps.index("if CurStep = ssDone"))
        self.assertNotIn("BeforeInstall: InstallDefaultApps", MAIN)

    def test_foreign_tree_and_discovery_values_are_refused(self):
        code = routine(DEFAULTS, "DefaultAppTreeMatches")
        self.assertIn("GetArrayLength(Names) <> ExpectedCount", code)
        self.assertIn("if not Known then Exit;", code)
        self.assertIn("if not DefaultAppTreeMatches(Child, Values) then Exit;", code)
        safe = routine(DEFAULTS, "DefaultAppProfileMatches")
        self.assertIn("DefaultAppTreeMatches(DefaultAppRoot, Values)", safe)
        self.assertIn("DefaultAppTreeMatches(DefaultAppProgIdKey, Values)", safe)
        self.assertIn("if not RegKeyExists(HKCU, DefaultAppRoot) or not RegKeyExists(HKCU, DefaultAppProgIdKey) then Exit;", safe)

    def test_snapshot_uses_same_value_list_before_payload(self):
        code = routine(RECOVERY, "RecoverySnapshotDefaultApps")
        self.assertIn("PopulateDefaultAppValues(Values);", code)
        self.assertIn("RecoverySnapshotValue(Values[I].Key, Values[I].Name);", code)
        self.assertIn(".DefaultAppValue := True;", code)
        begin = routine(RECOVERY, "RecoveryBegin")
        self.assertLess(begin.index("RecoverySnapshotDefaultApps;"), begin.index("ForceDirectories("))

    def test_recovery_only_restores_recorded_unchanged_writes(self):
        install = routine(DEFAULTS, "InstallDefaultApps")
        self.assertIn("AddDefaultAppValue(DefaultAppWrites,", install)
        code = routine(RECOVERY, "RecoveryRestore")
        guard = code[code.index("if RecoveryValues[I].DefaultAppValue then begin"):]
        self.assertLess(guard.index("if WriteIndex < 0 then Continue;"), guard.index("DefaultAppWrites[WriteIndex].Text"))
        self.assertLess(guard.index("DefaultAppWrites[WriteIndex].Text"), guard.index("RegWriteStringValue"))
        self.assertIn("Changed default-app registration retained:", guard)

    def test_uninstall_checks_owner_and_preserves_foreign_values(self):
        code = routine(DEFAULTS, "RemoveOwnedDefaultApps")
        self.assertIn("RootOwned := MatchesValue(DefaultAppRoot, 'OwnerId', OwnerId)", code)
        self.assertIn("MatchesValue(DefaultAppRoot, 'OwnerPath', InstalledExe)", code)
        self.assertIn("MatchesValue(DefaultAppProgIdKey, 'OwnerPath', InstalledExe)", code)
        self.assertIn("DefaultAppProgIdKey + '\\shell\\open\\command', '', MenuCommand(InstalledExe)", code)
        self.assertIn("DeleteMatching(Values[I].Key, Values[I].Name, Values[I].Text)", code)
        self.assertIn("PruneDefaultAppKeys(RootOwned, ProgIdOwned)", code)
        prune = routine(DEFAULTS, "PruneDefaultAppKeys")
        self.assertNotIn("'Software\\RegisteredApplications'", prune)
        self.assertNotIn("'Software\\Classes'", prune)
        self.assertNotIn("'Software\\LightImageTool'", prune)
        self.assertNotIn("RegDeleteKeyIncludingSubkeys", prune)
        self.assertIn("RemoveOwnedDefaultApps;", routine(MAIN, "CurUninstallStepChanged"))

    def test_existing_context_menu_remains_optional(self):
        registry = MAIN.split("[Registry]\n", 1)[1].split("[Code]", 1)[0]
        self.assertNotIn("Root:", registry)  # tracked code, not unguarded Inno writes
        install = routine(MENUS, "InstallMenu")
        self.assertIn("if WizardIsTaskSelected('explorer') then begin", install)
        self.assertIn("AddDefaultAppValue(DefaultAppWrites,", install)
        self.assertIn("RecordProductDeletion(", install)
        for ext in (".jpg", ".jpeg", ".png"):
            self.assertIn(f"InstallMenu('{ext}');", routine(MENUS, "InstallMenus"))
            self.assertIn(f"RemoveOwnedMenu('{ext}');", MAIN)

    def test_same_text_expand_string_is_not_an_owned_value(self):
        code = routine(MAIN, "MatchesValue")
        self.assertIn("Result := False;", code)
        self.assertEqual(code.count("QueryHkcuValueKind(Key, Name, Kind)"), 2)
        self.assertLess(code.index("if Kind <> 1 then Exit;"), code.index("RegQueryStringValue("))
        self.assertLess(code.index("RegQueryStringValue("), code.rindex("QueryHkcuValueKind("))
        self.assertIn("if Value <> Expected then Exit;", code)
        self.assertIn("Result := Kind = 1;", code)

    def test_raw_type_query_uses_install_registry_view(self):
        access = routine(REGISTRY, "ProductRegistryReadAccess")
        self.assertIn("Result := $0001;", access)
        self.assertIn("if Is64BitInstallMode then Result := Result or $0100", access)
        self.assertIn("else if IsWin64 then Result := Result or $0200", access)
        code = routine(REGISTRY, "QueryHkcuValueKind")
        self.assertIn("ProductRegOpen($80000001, Key, 0, ProductRegistryReadAccess, Handle)", code)
        self.assertIn("Result := False;", code)
        self.assertIn("if ProductRegQuery(Handle, Name, 0, Kind, 0, Size) <> 0 then Exit;", code)
        self.assertIn("finally ProductRegClose(Handle); end;", code)
        self.assertLess(MAIN.index('#include "registry-values.iss"'), MAIN.index("function MatchesValue"))

    def test_snapshot_rechecks_exact_string_type_and_text(self):
        code = routine(RECOVERY, "RecoverySnapshotValue")
        self.assertIn("if not QueryHkcuValueKind(Key, Name, Kind) then", code)
        self.assertIn("if not MatchesValue(Key, Name, RecoveryValues[I].Text) then", code)
        self.assertNotIn("RecoveryRegOpen", RECOVERY)

    def test_shared_registered_applications_parent_is_never_pruned(self):
        code = routine(RECOVERY, "RecoveryRestore")
        prune = code.split("for I := GetArrayLength(RecoveryValues)-1 downto 0 do begin", 1)[1]
        generic = prune.split("PruneDefaultAppKeys(", 1)[0]
        self.assertIn("if not RecoveryValues[I].DefaultAppValue then\n      RegDeleteKeyIfEmpty(HKCU, RecoveryValues[I].Key);", generic)
        self.assertNotIn("DefaultAppWriteIndex", generic)
        self.assertNotIn(" or", generic)
        specific = routine(DEFAULTS, "PruneDefaultAppKeys")
        self.assertNotIn("RegisteredApplications", specific)
        self.assertTrue(all(
            "DefaultAppRoot" in line or "DefaultAppProgIdKey" in line
            for line in specific.splitlines() if "RegDeleteKeyIfEmpty(" in line
        ))


if __name__ == "__main__":
    unittest.main(verbosity=2)
