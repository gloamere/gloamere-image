"""Linux-safe installer source/model contracts. NOT a Windows/Inno execution test."""
import json
import copy
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAIN = (ROOT / 'installer/light-image-tool.iss').read_text('utf-8')
DEFAULTS = (ROOT / 'installer/default-apps.iss').read_text('utf-8')
MENUS = (ROOT / 'installer/menus.iss').read_text('utf-8')
MIGRATION = (ROOT / 'installer/migration.iss').read_text('utf-8')
NATIVE = (ROOT / "src/updater/installer_native.rs").read_text("utf-8")
INTEGRATION = (ROOT / "src/updater/integration.rs").read_text("utf-8")
RECOVERY = (ROOT / 'installer/recovery.iss').read_text('utf-8')
LEGACY_NAMES = ('light-image-tool.exe', 'light-provenance-worker.exe')
V04_FIXTURE = json.loads((ROOT / 'tests/fixtures/installer/branded-v0.4.json').read_text('utf-8'))


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


def profile_model(version):
    """Deterministic typed-value model, checked against source literals below.

    This deliberately does not execute Inno/Pascal or a Windows registry API.
    """
    values = copy.deepcopy(V04_FIXTURE['candidate_values'])
    if version == 'v04':
        return values
    if version != 'v05':
        raise ValueError('Unknown fixture profile')
    code = routine(DEFAULTS, 'PopulateDefaultAppProfile')
    descriptions = re.findall(r"Description := '([^']*)';", code)
    labels = re.findall(r"ImageLabel := '([^']*)';", code)
    for value in values:
        if value['name'] == 'ApplicationDescription':
            value['text'] = descriptions[1]
        if value['key'] == r'Software\Classes\LightImageTool.Image' and not value['name']:
            value['text'] = 'gloamere-image' + labels[1]
    associations = r'Software\LightImageTool\DefaultApps\Capabilities\FileAssociations'
    for ext in ('.gif', '.webp'):
        values.insert(-1, {'key': associations, 'name': ext, 'type': 'REG_SZ', 'text': 'LightImageTool.Image'})
    return values


def typed_values(values):
    return {(value['key'], value['name']): (value['type'], value['text']) for value in values}


class V04ToV05FixtureContracts(unittest.TestCase):
    def test_frozen_predecessor_identity_is_retained(self):
        identity = V04_FIXTURE['identity']
        self.assertEqual(V04_FIXTURE['baseline_version'], '0.4.0')
        for text in (identity['product_id'], identity['default_directory'], identity['owner_marker'],
                     identity['main_executable'], identity['worker_executable']):
            self.assertIn(text, MAIN)
        self.assertIn('#define MenuVerb "' + identity['menu_verb'] + '"', MAIN)
        self.assertIn(identity['progid'], DEFAULTS)
        self.assertIn('UsePreviousAppDir=yes', MAIN)
        self.assertIn('UsePreviousTasks=yes', MAIN)
        settings = (ROOT / 'src/settings.rs').read_text('utf-8')
        self.assertIn('std::env::var_os("LOCALAPPDATA")', settings)
        self.assertIn('.join("LightImageTool")', settings)
        self.assertIn('.join("settings.json")', settings)
        self.assertEqual(V04_FIXTURE['settings']['schema_version'], identity['settings_schema'])

    def test_v04_fixture_is_exactly_the_historical_source_profile(self):
        values = profile_model('v04')
        self.assertEqual(len(values), 14)
        self.assertEqual(len(typed_values(values)), 14)
        self.assertTrue(all(value['type'] == 'REG_SZ' for value in values))
        code = routine(DEFAULTS, 'PopulateDefaultAppProfile')
        old = typed_values(values)
        root = r'Software\LightImageTool\DefaultApps'
        self.assertIn("Result := '" + root + "';", routine(DEFAULTS, 'DefaultAppRoot'))
        self.assertIn("Description := '" + old[(root + r'\Capabilities', 'ApplicationDescription')][1] + "';", code)
        self.assertIn("ImageLabel := ' JPEG / PNG image';", code)
        self.assertIn("if Profile = dapLegacy then begin\n    Path := LegacyInstalledExe", code)
        self.assertIn("LabelName := ProductName; Icon :=", code)

    def test_upgrade_delta_only_adds_formats_and_updates_owned_labels(self):
        old, new = typed_values(profile_model('v04')), typed_values(profile_model('v05'))
        associations = r'Software\LightImageTool\DefaultApps\Capabilities\FileAssociations'
        self.assertEqual(set(new) - set(old), {(associations, '.gif'), (associations, '.webp')})
        self.assertEqual(set(old) - set(new), set())
        self.assertEqual({key for key in old if old[key] != new[key]}, {
            (r'Software\LightImageTool\DefaultApps\Capabilities', 'ApplicationDescription'),
            (r'Software\Classes\LightImageTool.Image', ''),
        })
        self.assertEqual(new[(r'Software\RegisteredApplications', 'gloamere-image')],
                         old[(r'Software\RegisteredApplications', 'gloamere-image')])
        self.assertIn('DefaultAppProfileMatches(dapV04)', routine(DEFAULTS, 'DefaultAppsSafe'))

    def test_union_snapshot_can_restore_exact_v04_absence_and_text(self):
        old, new = typed_values(profile_model('v04')), typed_values(profile_model('v05'))
        unrelated = typed_values(V04_FIXTURE['preserved_values'])
        initial = {**old, **unrelated}
        snapshot = {key: initial.get(key) for key in new}
        current = {**initial, **new}
        for key, value in snapshot.items():
            self.assertEqual(current[key], new[key])
            if value is None:
                del current[key]
            else:
                current[key] = value
        self.assertEqual(current, initial)
        self.assertIn("before: Option<RawValue>", INTEGRATION)
        self.assertIn("Registry restore readback changed", INTEGRATION)

    def test_partial_hybrid_extra_or_differently_typed_profiles_are_not_exact(self):
        old, new = typed_values(profile_model('v04')), typed_values(profile_model('v05'))
        key = (r'Software\LightImageTool\DefaultApps\Capabilities\FileAssociations', '.gif')
        broken = [dict(new), dict(old), dict(new), dict(new)]
        del broken[0][key]
        broken[1][key] = new[key]
        broken[2][key] = ('REG_EXPAND_SZ', new[key][1])
        broken[3][(key[0], '.avif')] = new[key]
        for profile in broken:
            self.assertNotIn(profile, (old, new))
        tree = routine(DEFAULTS, 'DefaultAppTreeMatches')
        self.assertIn('GetArrayLength(Names) <> ExpectedCount', tree)
        self.assertIn('if not Known then Exit;', tree)
        self.assertIn('MatchesValue(Key, Values[I].Name, Values[I].Text)', tree)

    def test_upgrade_never_adds_settings_or_ordinary_files_to_installer_paths(self):
        self.assertNotIn('[InstallDelete]', MAIN)
        self.assertNotIn('[UninstallDelete]', MAIN)
        for name in (*V04_FIXTURE['ordinary_files'], 'settings.json'):
            self.assertNotIn(name, MAIN + MIGRATION + RECOVERY)
        source = '\n'.join(line for line in (MAIN + DEFAULTS + MENUS + RECOVERY).splitlines()
                           if not line.lstrip().startswith(('//', ';')))
        self.assertNotIn('UserChoice', source)
        self.assertNotRegex(source, r'Software\\Classes\\\.')


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
                      '#define ProductWorker "runtime\\gloamere-image-worker.exe"',
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
        for action in ('InstallMenus;', 'InstallDefaultApps;'):
            self.assertLess(steps.index('RecoveryStart;'), steps.index(action))
            self.assertLess(steps.index(action), steps.index('if CurStep = ssDone'))
        self.assertIn('if CurStep = ssPostInstall then begin', steps)
        self.assertIn('RecoveryInstalled;', steps)
        self.assertNotIn('[InstallDelete]', MAIN)

    def test_known_pair_and_only_verified_hashes_are_deleted(self):
        self.assertIn('Only the owned v0.6.0 bootstrap', NATIVE)
        self.assertIn('Legacy executable pair is incomplete', NATIVE)
        self.assertIn('Foreign, changed or missing owned file', NATIVE)
        self.assertIn('Obsolete file changed during install', NATIVE)
        self.assertNotIn('MigrateLegacyPayload;', routine(MAIN, 'CurStepChanged'))
        self.assertIn("NativeCommand('prepare'", routine(RECOVERY, 'RecoveryBegin'))

    def test_old_and_new_processes_must_be_closed(self):
        code = routine(MAIN, 'PrepareToInstall')
        for name in ('InstalledExe', 'WorkerName', 'LegacyExeName', 'LegacyWorkerName'):
            self.assertIn(name, code)
        self.assertIn('CloseApplications=no', MAIN)

    def test_existing_install_and_old_profiles_require_exact_string_ownership(self):
        for name in ('PrepareToInstall', 'InitializeUninstall', 'LegacyInstallationOwned'):
            self.assertIn("MatchesValue(UninstallKey, 'InstallLocation'", routine(MAIN, name))
        self.assertIn('if (Profile <> dapV05) and not LegacyInstallationOwned then Exit;', routine(DEFAULTS, 'DefaultAppProfileMatches'))
        self.assertIn('if Legacy and not LegacyInstallationOwned then Exit;', routine(MENUS, 'MenuProfileMatches'))

    def test_complete_profiles_refuse_partial_mixed_or_duplicate_discovery(self):
        code = routine(DEFAULTS, 'DefaultAppProfileMatches')
        self.assertIn('not RegKeyExists(HKCU, DefaultAppRoot) or not RegKeyExists(HKCU, DefaultAppProgIdKey)', code)
        self.assertIn("RegValueExists(HKCU, 'Software\\RegisteredApplications', OtherName)", code)
        for tree in ('DefaultAppRoot', 'DefaultAppProgIdKey'):
            self.assertIn(f'not DefaultAppTreeMatches({tree}, Values)', code)
        safe = routine(DEFAULTS, 'DefaultAppsSafe')
        for profile in ('dapLegacy', 'dapV04', 'dapV05'):
            self.assertIn(f'DefaultAppProfileMatches({profile})', safe)
        menu = routine(MENUS, 'MenuProfileMatches')
        self.assertIn('DefaultAppTreeMatches(MenuKey(Ext), Values)', menu)

    def test_legacy_discovery_deletion_is_recorded_after_success(self):
        code = routine(DEFAULTS, 'InstallDefaultApps')
        self.assertLess(code.index('MatchesValue('), code.index('RegDeleteValue('))
        self.assertLess(code.index('RegDeleteValue('), code.index('RecordProductDeletion('))
        self.assertIn('"Light Image Tool"', INTEGRATION)
        self.assertIn('DISCOVERY', INTEGRATION)

    def test_deleted_registry_values_restore_only_while_absent(self):
        self.assertIn('fn value_admissible', INTEGRATION)
        self.assertIn('record.expected', INTEGRATION)
        self.assertIn('record.before', INTEGRATION)
        self.assertIn('Foreign or unsealed registry change retained', INTEGRATION)
        self.assertIn('RecordProductDeletion(', routine(MENUS, 'InstallMenu'))

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
        self.assertIn('programs_directory()', INTEGRATION)
        self.assertIn('Foreign product-named shortcut', INTEGRATION)

    def test_shortcut_snapshot_restore_and_conflict_preservation(self):
        self.assertIn('struct FileImage', INTEGRATION)
        self.assertIn('sha256: String', INTEGRATION)
        self.assertIn('platform::FilePermissions', INTEGRATION)
        self.assertIn('fn shortcut_admissible', INTEGRATION)
        self.assertIn('Foreign or unsealed shortcut retained', INTEGRATION)
        self.assertIn('Restored shortcut bytes/permissions/presence differ', INTEGRATION)
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
