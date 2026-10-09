// Explicit recovery of overwritten files: Inno's ordinary rollback is insufficient.
type
  TRecoveryFile = record
    Relative, OriginalHash: String;
    Existed: Boolean;
  end;
  TRecoveryValue = record
    Key, Name, Text: String;
    Existed, DefaultAppValue: Boolean;
    Kind, Number: LongWord;
  end;
var
  RecoveryFiles: array of TRecoveryFile;
  RecoveryValues: array of TRecoveryValue;
  RecoveryPaths: TStringList;
  RecoveryDirectory, RecoveryApp, RecoveryErrors: String;
  RecoveryReady, RecoveryStarted, RecoverySuccess, RecoveryUninstallExisted: Boolean;

