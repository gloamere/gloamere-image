// Fixed destinations only. No manifest-provided or user-supplied deletion paths.
type
  TRecoveryShortcut = record
    Path, BackupName, OriginalHash: String;
    Existed, Legacy: Boolean;
  end;
var
  LegacyHashes: array[0..1] of String;
  LegacyPayloadPresent, LegacyShortcutOwned: Boolean;
  RecoveryShortcuts: array of TRecoveryShortcut;
