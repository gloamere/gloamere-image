"""Linux-safe source contracts, NOT an Inno compile or Windows lifecycle test."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAIN = (ROOT / "installer/light-image-tool.iss").read_text("utf-8")
DEFAULTS = (ROOT / "installer/default-apps.iss").read_text("utf-8")
MENUS = (ROOT / "installer/menus.iss").read_text("utf-8")
NATIVE = (ROOT / "src/updater/installer_native.rs").read_text("utf-8")
INTEGRATION = (ROOT / "src/updater/integration.rs").read_text("utf-8")
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
            set(entries[0].split("Flags: ", 1)[1].split(";", 1)[0].split()),
            {"postinstall", "unchecked", "shellexec", "skipifsilent", "nowait"},
        )
        self.assertIn("Check: NativeCanOpenDefaults",entries[0])
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

    def test_handler_claims_exact_supported_formats_in_current_profile(self):
        code = routine(DEFAULTS, "PopulateDefaultAppProfile")
        claims = re.findall(
            r"AddDefaultAppValue\(Values, Capabilities \+ '\\FileAssociations', '([^']+)', '([^']+)'\);",
            code,
        )
        self.assertEqual(
            claims,
            [(ext, "LightImageTool.Image") for ext in (".jpg", ".jpeg", ".png", ".gif", ".webp")],
        )
        self.assertEqual(code.count("AddDefaultAppValue(Values,"), 16)
        self.assertIn("if Profile = dapV05 then begin\n    AddDefaultAppValue(Values, Capabilities", code)
        self.assertIn("PopulateDefaultAppProfile(Values, dapV05);", routine(DEFAULTS, "PopulateDefaultAppValues"))
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
        self.assertIn("RecoveryStart;", steps)
        self.assertLess(steps.index("RecoveryStart;"), steps.index("InstallDefaultApps;"))
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
        self.assertIn("expected_values", INTEGRATION)
        self.assertIn("Self::capture_with", INTEGRATION)
        self.assertIn("before: Option<RawValue>", INTEGRATION)
        self.assertIn("integration::Snapshot::capture", NATIVE)
        begin = routine(RECOVERY, "RecoveryBegin")
        self.assertLess(begin.index("NativeCommand('prepare'"), begin.index("RecoveryReady := True"))

    def test_recovery_only_restores_recorded_unchanged_writes(self):
        self.assertIn("fn value_admissible", INTEGRATION)
        self.assertIn("self.write_intent", INTEGRATION)
        self.assertIn("record.key", INTEGRATION)
        self.assertIn("DISCOVERY", INTEGRATION)
        self.assertIn("Registry changed immediately before restore", INTEGRATION)
        self.assertIn("Restored registry raw type/bytes differ", INTEGRATION)
        self.assertIn("self.check_live(backend)?", INTEGRATION)

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
        for ext in (".jpg", ".jpeg", ".png", ".gif", ".webp"):
            self.assertIn(f"InstallMenu('{ext}');", routine(MENUS, "InstallMenus"))
            self.assertIn(f"RemoveOwnedMenu('{ext}');", MAIN)

    def test_all_menu_formats_share_preflight_snapshot_and_recovery(self):
        for ext in (".jpg", ".jpeg", ".png", ".gif", ".webp"):
            for name in ("PrepareToInstall", "CurStepChanged"):
                self.assertIn(f"not MenuSafe('{ext}')", routine(MAIN, name))
            self.assertIn('"' + ext + '"', INTEGRATION)
        self.assertIn("fn menu_key", INTEGRATION)
        self.assertIn("for ext in EXTENSIONS", INTEGRATION)
        self.assertIn("fn verify_before", INTEGRATION)

    def test_predecessor_menu_profiles_never_claim_new_formats(self):
        match = routine(MENUS, "MenuProfileMatches")
        self.assertIn("if Legacy and (Ext <> '.jpg') and (Ext <> '.jpeg') and (Ext <> '.png') then Exit;", match)
        self.assertLess(match.index("Ext <> '.png'"), match.index("PopulateMenuValues"))

    def test_v04_candidate_values_remain_an_exact_accepted_profile(self):
        match = routine(DEFAULTS, "DefaultAppProfileMatches")
        self.assertIn("if (Profile <> dapV05) and not LegacyInstallationOwned then Exit;", match)
        self.assertIn("PopulateDefaultAppProfile(Values, Profile)", match)
        safe = routine(DEFAULTS, "DefaultAppsSafe")
        for profile in ("dapLegacy", "dapV04", "dapV05"):
            self.assertIn(f"DefaultAppProfileMatches({profile})", safe)
        profile = routine(DEFAULTS, "PopulateDefaultAppProfile")
        self.assertIn("Description := 'Local JPEG and PNG image viewer and editor.';", profile)
        self.assertIn("ImageLabel := ' JPEG / PNG image';", profile)
        self.assertIn("if Profile = dapLegacy then begin", profile)

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
        self.assertIn("pub struct RawValue", INTEGRATION)
        self.assertIn("pub kind: u32", INTEGRATION)
        self.assertIn("pub bytes: Vec<u8>", INTEGRATION)
        self.assertIn("RegQueryValueExW", INTEGRATION)
        self.assertIn("Registry restore readback changed", INTEGRATION)

    def test_shared_registered_applications_parent_is_never_pruned(self):
        keys = INTEGRATION.split("fn fixed_keys()", 1)[1].split("fn string_path", 1)[0]
        self.assertNotIn("DISCOVERY", keys)
        specific = routine(DEFAULTS, "PruneDefaultAppKeys")
        self.assertNotIn("RegisteredApplications", specific)
        self.assertTrue(all("DefaultAppRoot" in line or "DefaultAppProgIdKey" in line
                            for line in specific.splitlines() if "RegDeleteKeyIfEmpty(" in line))



if __name__ == "__main__":
    unittest.main(verbosity=2)
