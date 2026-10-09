"""Linux-safe installer source/model contracts. NOT a Windows/Inno execution test."""
import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAIN = (ROOT / 'installer/light-image-tool.iss').read_text('utf-8')
DEFAULTS = (ROOT / 'installer/default-apps.iss').read_text('utf-8')
MENUS = (ROOT / 'installer/menus.iss').read_text('utf-8')
MIGRATION = (ROOT / 'installer/migration.iss').read_text('utf-8')
RECOVERY = (ROOT / 'installer/recovery.iss').read_text('utf-8')
LEGACY_NAMES = ('light-image-tool.exe', 'light-provenance-worker.exe')


def routine(source, name):
    found = re.search(rf'^(?:procedure|function) {name}\b.*?(?=^(?:procedure|function) |\Z)',
                      source, re.M | re.S)
    if not found:
        raise AssertionError(f'Missing routine: {name}')
    return found.group()


def canonical_hash_model(text, name):
    """Independent model of the deliberately narrow legacy manifest reader."""
    if name not in LEGACY_NAMES or not 0 < len(text.encode()) <= 4194304:
        return None
    lines = text.splitlines()
    if not 5 <= len(lines) <= 40000:
        return None
    matches = []
    for i in range(1, len(lines) - 3):
        if len(lines[i]) > 4096:
            return None
        if lines[i].strip() != f'"path": "{name}",':
            continue
        if lines[i - 1].strip() != '{':
            return None
        if not re.fullmatch(r'"size_bytes": [0-9]{1,12},', lines[i + 1].strip()):
            return None
        value = lines[i + 2].strip()
        match = re.fullmatch(r'"sha256": "([0-9a-f]{64})"', value)
        if not match or lines[i + 3].strip() not in ('}', '},'):
            return None
        matches.append(match[1])
    return matches[0] if len(matches) == 1 else None


class ManifestContract(unittest.TestCase):
    def setUp(self):
        self.hash = 'b' * 64
        self.record = {'path': LEGACY_NAMES[0], 'size_bytes': 123, 'sha256': self.hash}

    def test_reads_the_actual_legacy_packager_shape(self):
        text = json.dumps({'version': '0.3.0', 'files': [self.record]}, indent=2)
        self.assertEqual(canonical_hash_model(text, LEGACY_NAMES[0]), self.hash)
        self.assertEqual(canonical_hash_model(text.replace('\n', '\r\n'), LEGACY_NAMES[0]), self.hash)
        code = routine(MIGRATION, 'LegacyManifestHash')
        self.assertIn('Copy(Value, 12, 64)', code)
        self.assertEqual(len('"sha256": "' + self.hash + '"'), 76)
        self.assertEqual(len('"sha256": "'), 11)
        size = '"size_bytes": 123,'
        self.assertEqual(size[14:len(size)-1], '123')

    def test_rejects_duplicate_records(self):
        text = json.dumps({'files': [self.record, self.record]}, indent=2)
        self.assertIsNone(canonical_hash_model(text, LEGACY_NAMES[0]))
        self.assertIn('if Found <> 1 then Exit;', MIGRATION)

    def test_rejects_arbitrary_paths(self):
        for name in ('../image.png', '/etc/passwd', 'other.exe', 'LIGHT-IMAGE-TOOL.EXE'):
            with self.subTest(name=name):
                text = json.dumps({'files': [{**self.record, 'path': name}]}, indent=2)
                self.assertIsNone(canonical_hash_model(text, name))

    def test_rejects_noncanonical_or_invalid_evidence(self):
        for record in ({**self.record, 'sha256': 'f' * 63},
                       {**self.record, 'sha256': 'F' * 64},
                       {**self.record, 'size_bytes': -1},
                       {**self.record, 'size_bytes': '123'},
                       {**self.record, 'extra': 'foreign'}):
            self.assertIsNone(canonical_hash_model(json.dumps({'files': [record]}, indent=2), LEGACY_NAMES[0]))
        self.assertIsNone(canonical_hash_model(json.dumps({'files': [self.record]}), LEGACY_NAMES[0]))

    def test_missing_record_never_grants_deletion(self):
        text = json.dumps({'files': [self.record]}, indent=2)
        self.assertIsNone(canonical_hash_model(text, LEGACY_NAMES[1]))
        code = routine(MIGRATION, 'MigrationPreflight')
        self.assertIn('not LegacyManifestHash(LegacyFileName(I), LegacyHashes[I])', code)
        self.assertIn('GetSHA256OfFile(Path) <> LegacyHashes[I]', code)

    def test_bounded_before_loading(self):
        code = routine(MIGRATION, 'LegacyManifestHash')
        self.assertLess(code.index('FileSize('), code.index('LoadStringsFromFile('))
        self.assertIn('ManifestSize > 4194304', code)
        self.assertIn('GetArrayLength(Lines) > 40000', code)


class BrandingMigrationContracts(unittest.TestCase):
    def test_public_brand_and_opaque_identity(self):
        for entry in ('AppName=gloamere-image', 'DefaultGroupName=gloamere-image',
                      '#define ProductExe "gloamere-image.exe"',
                      '#define ProductWorker "gloamere-image-worker.exe"',
                      'OutputBaseFilename=gloamere-image-', 'UsePreviousAppDir=yes',
                      'DefaultDirName={userpf}\\LightImageTool',
                      '9CCB6F43-7782-4F69-9E91-67C6D144737B', 'LIGHT-IMAGE-INSTALL.txt'):
            self.assertIn(entry, MAIN)
        self.assertIn('LightImageTool.Image', DEFAULTS)

    def test_all_installer_icon_surfaces_use_the_product_asset(self):
        for entry in ('SetupIconFile={#PayloadDir}\\assets\\gloamere-image.ico',
                      'UninstallDisplayIcon={app}\\assets\\gloamere-image.ico',
                      'IconFilename: "{app}\\assets\\gloamere-image.ico"'):
            self.assertIn(entry, MAIN)
        self.assertIn('Icon := InstalledIcon', MENUS)
        self.assertIn("InstalledIcon + '\",0'", DEFAULTS)

    def test_preflight_and_snapshot_precede_all_mutation(self):
        prepare = routine(MAIN, 'PrepareToInstall')
        self.assertLess(prepare.index('MigrationPreflight'), prepare.index('RecoveryBegin'))
        self.assertLess(prepare.index("not MenuSafe('.jpg')"), prepare.index('RecoveryBegin'))
        steps = routine(MAIN, 'CurStepChanged')
        for action in ('MigrateLegacyPayload;', 'InstallMenus;', 'InstallDefaultApps;'):
            self.assertLess(steps.index('RecoveryStarted := True;'), steps.index(action))
            self.assertLess(steps.index(action), steps.index('if CurStep = ssDone'))
        self.assertNotIn('ssPostInstall', steps)
        self.assertNotIn('[InstallDelete]', MAIN)

    def test_known_pair_and_only_verified_hashes_are_deleted(self):
        code = routine(MIGRATION, 'MigrateLegacyPayload')
        self.assertIn('if not RecoveryReady then RaiseException', code)
        self.assertIn('if not LegacyInstallationOwned then RaiseException', code)
        self.assertIn('for I := 0 to 1 do begin', code)
        self.assertLess(code.index('GetSHA256OfFile(Path) <> LegacyHashes[I]'), code.index('DeleteFile(Path)'))
        for entry in ('FileAvailable(Path)', 'MigrationRegularPath(Path)'):
            self.assertIn(entry, code)
        begin = routine(RECOVERY, 'RecoveryBegin')
        self.assertIn('RecoveryPaths.Add(LegacyExeName)', begin)
        self.assertIn('RecoveryPaths.Add(LegacyWorkerName)', begin)
        self.assertNotIn('LegacyManifestHash', begin)

    def test_old_and_new_processes_must_be_closed(self):
        code = routine(MAIN, 'PrepareToInstall')
        for name in ('InstalledExe', 'WorkerName', 'LegacyExeName', 'LegacyWorkerName'):
            self.assertIn(name, code)
        self.assertIn('CloseApplications=no', MAIN)

    def test_existing_install_and_old_profiles_require_exact_string_ownership(self):
        for name in ('PrepareToInstall', 'InitializeUninstall', 'LegacyInstallationOwned'):
            self.assertIn("MatchesValue(UninstallKey, 'InstallLocation'", routine(MAIN, name))
        self.assertIn('if not LegacyInstallationOwned then Exit;', routine(DEFAULTS, 'DefaultAppProfileMatches'))
        self.assertIn('if Legacy and not LegacyInstallationOwned then Exit;', routine(MENUS, 'MenuProfileMatches'))

    def test_complete_profiles_refuse_partial_mixed_or_duplicate_discovery(self):
        code = routine(DEFAULTS, 'DefaultAppProfileMatches')
        self.assertIn('not RegKeyExists(HKCU, DefaultAppRoot) or not RegKeyExists(HKCU, DefaultAppProgIdKey)', code)
        self.assertIn("RegValueExists(HKCU, 'Software\\RegisteredApplications', OtherName)", code)
        for tree in ('DefaultAppRoot', 'DefaultAppProgIdKey'):
            self.assertIn(f'not DefaultAppTreeMatches({tree}, Values)', code)
        safe = routine(DEFAULTS, 'DefaultAppsSafe')
        self.assertIn('DefaultAppProfileMatches(False) or DefaultAppProfileMatches(True)', safe)
        menu = routine(MENUS, 'MenuProfileMatches')
        self.assertIn('DefaultAppTreeMatches(MenuKey(Ext), Values)', menu)

    def test_legacy_discovery_deletion_is_recorded_after_success(self):
        code = routine(DEFAULTS, 'InstallDefaultApps')
        self.assertLess(code.index('MatchesValue('), code.index('RegDeleteValue('))
        self.assertLess(code.index('RegDeleteValue('), code.index('RecordProductDeletion('))
        snapshot = routine(RECOVERY, 'RecoverySnapshotDefaultApps')
        self.assertIn("'Software\\RegisteredApplications', LegacyProductName", snapshot)

    def test_deleted_registry_values_restore_only_while_absent(self):
        code = routine(RECOVERY, 'RecoveryRestore')
        deleted = code.split('if DefaultAppWrites[WriteIndex].Removed then begin', 1)[1].split('end else if', 1)[0]
        self.assertIn('if RegValueExists(', deleted)
        self.assertIn('Continue;', deleted)
        self.assertLess(code.index('DefaultAppWrites[WriteIndex].Removed'), code.index('if RecoveryValues[I].Existed then begin'))
        self.assertIn('RecordProductDeletion(', routine(MENUS, 'InstallMenu'))
        self.assertIn('.DefaultAppValue := True;', routine(RECOVERY, 'RecoverySnapshotMenu'))

    def test_shortcut_destinations_are_fixed_and_verified(self):
        code = routine(MIGRATION, 'ProductShortcut')
        self.assertEqual(code.count('ExpandConstant('), 2)
        self.assertIn('{userprograms}\\Light Image Tool\\Light Image Tool.lnk', code)
        self.assertIn('{userprograms}\\gloamere-image\\gloamere-image.lnk', code)
        own = routine(MIGRATION, 'ShortcutOwned')
        self.assertIn('Path <> ProductShortcut(Legacy)', own)
        self.assertIn('CompareText(Link.TargetPath, Target) = 0', own)
        self.assertIn("Link.Arguments = ''", own)
        self.assertNotIn('Link.Save', own)
        preflight = routine(MIGRATION, 'MigrationPreflight')
        self.assertIn('FileOrDirExists(ProductShortcut(True)) and not LegacyShortcutOwned', preflight)
        self.assertIn('not MarkerOwned or not ShortcutOwned(ProductShortcut(False), False)', preflight)
        check = routine(RECOVERY, 'RecoveryShortcutCheck')
        self.assertIn('.Path = ProductShortcut(', check)
        self.assertIn('MigrationRegularPath(', check)

    def test_shortcut_snapshot_restore_and_conflict_preservation(self):
        begin = routine(RECOVERY, 'RecoveryBegin')
        self.assertLess(begin.index('RecoverySnapshotShortcut(False)'), begin.index('RecoveryReady := True'))
        self.assertIn('RecoverySnapshotShortcut(True)', begin)
        restore = routine(RECOVERY, 'RecoveryRestoreShortcuts')
        self.assertIn('Changed shortcut retained:', restore)
        self.assertIn('GetSHA256OfFile(Backup) <> RecoveryShortcuts[I].OriginalHash', restore)
        self.assertIn('if not RecoveryRestoreShortcuts then Result := False;', routine(RECOVERY, 'RecoveryRestore'))
        uninstall = routine(MAIN, 'InitializeUninstall')
        self.assertIn('not ShortcutOwned(ProductShortcut(False), False)', uninstall)
        self.assertIn('not ShortcutOwned(ProductShortcut(True), True)', uninstall)

    def test_shortcut_aliases_do_not_authorize_inno_deletion(self):
        code = routine(MIGRATION, 'ShortcutCompanionsSafe')
        for name in ("Stem + '.pif'", "Stem + '.url'", "FileOrDirExists(Stem)"):
            self.assertIn(name, code)
        self.assertIn('ShortcutCompanionsSafe(False)', routine(MIGRATION, 'MigrationPreflight'))
        self.assertIn('ShortcutCompanionsSafe(False)', routine(MIGRATION, 'MigrateLegacyPayload'))
        for legacy in ('False', 'True'):
            self.assertIn(f'ShortcutCompanionsSafe({legacy})', routine(MAIN, 'InitializeUninstall'))
        self.assertIn('Flags: uninsneveruninstall', MAIN)
        self.assertIn('RemoveOwnedCurrentShortcut;', routine(MAIN, 'CurUninstallStepChanged'))
        owned = routine(MIGRATION, 'RemoveOwnedCurrentShortcut')
        self.assertLess(owned.index('ShortcutOwned(Path, False)'), owned.index('DeleteFile(Path)'))

    def test_no_broad_deletion_or_default_takeover(self):
        source = '\n'.join((MAIN, DEFAULTS, MENUS, MIGRATION, RECOVERY))
        code = '\n'.join(line for line in source.splitlines() if not line.lstrip().startswith(('//', ';')))
        for forbidden in ('RegDeleteKeyIncludingSubkeys', 'DelTree(', 'UserChoice', 'SetAppAsDefault', '[UninstallDelete]'):
            self.assertNotIn(forbidden, code)
        self.assertNotRegex(code, r'\b(?:HKLM|HKCR)\b')
        self.assertNotRegex(code, r'Software\\Classes\\\.')


class VisibleBrandingContract(unittest.TestCase):
    def test_current_chinese_labels_and_app_titles_use_new_brand(self):
        messages = (ROOT / 'installer/chinese-simplified.isl').read_text('utf-8')
        app = (ROOT / 'src/app.rs').read_text('utf-8')
        main = (ROOT / 'src/main.rs').read_text('utf-8')
        for text in (messages, app, main):
            self.assertNotIn('轻图', text)
            self.assertNotIn('Light Image Tool', text)
            self.assertIn('gloamere-image', text)

    def test_taskbar_process_and_shortcut_identity_match_before_ui(self):
        main = (ROOT / 'src/main.rs').read_text('utf-8')
        self.assertIn('"LightImageTool.Image"', main)
        self.assertIn('AppUserModelID: "LightImageTool.Image"', MAIN)
        self.assertIn('SetCurrentProcessExplicitAppUserModelID', main)
        self.assertLess(main.index('    configure_taskbar_identity();'), main.index('rfd::MessageDialog::new()'))
        self.assertIn('.with_icon(viewer_ui::window_icon())', main)


if __name__ == '__main__':
    unittest.main(verbosity=2)
