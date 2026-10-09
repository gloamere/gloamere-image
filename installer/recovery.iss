#include PayloadRecoveryList

function RecoveryJson(S: String): String;
var I: Integer;
begin
  Result := '"';
  for I := 1 to Length(S) do begin
    if S[I] = '"' then Result := Result + '\"'
    else if S[I] = '\' then Result := Result + '\\'
    else if Ord(S[I]) < 32 then Result := Result + '\u' + Format('%.4x', [Ord(S[I])])
    else Result := Result + S[I];
  end;
  Result := Result + '"';
end;

procedure RecoverySnapshotValue(Key, Name: String);
var I: Integer; Kind: LongWord;
begin
  I := GetArrayLength(RecoveryValues);
  SetArrayLength(RecoveryValues, I+1);
  RecoveryValues[I].Key := Key; RecoveryValues[I].Name := Name;
  RecoveryValues[I].DefaultAppValue := False;
  RecoveryValues[I].Existed := RegValueExists(HKCU, Key, Name);
  if not RecoveryValues[I].Existed then Exit;
  if not QueryHkcuValueKind(Key, Name, Kind) then
    RaiseException('Cannot inspect the product registry value.');
  RecoveryValues[I].Kind := Kind;
  if Kind = 1 then begin
    if not RegQueryStringValue(HKCU, Key, Name, RecoveryValues[I].Text) then
      RaiseException('Cannot snapshot product text.');
    if Length(RecoveryValues[I].Text) > 65536 then
      RaiseException('Product registry text exceeds recovery bounds.');
    if not MatchesValue(Key, Name, RecoveryValues[I].Text) then
      RaiseException('Product registry text or type changed during snapshot; no payload was changed.');
  end else if Kind = 4 then begin
    if not RegQueryDWordValue(HKCU, Key, Name, RecoveryValues[I].Number) then
      RaiseException('Cannot snapshot product number.');
  end else RaiseException('Unsupported value type in the owned product key; no payload was changed.');
end;

procedure RecoverySnapshotMenu(Ext: String);
var Values: TDefaultAppValues; I: Integer;
begin
  PopulateMenuValues(Values, Ext, False);
  for I := 0 to GetArrayLength(Values)-1 do begin
    RecoverySnapshotValue(Values[I].Key, Values[I].Name);
    RecoveryValues[GetArrayLength(RecoveryValues)-1].DefaultAppValue := True;
  end;
end;

procedure RecoverySnapshotDefaultApps;
var Values: TDefaultAppValues; I: Integer;
begin
  PopulateDefaultAppValues(Values);
  // New/old profile share all tree values; discovery names are the only union delta.
  AddDefaultAppValue(Values, 'Software\RegisteredApplications', LegacyProductName,
    DefaultAppRoot + '\Capabilities');
  for I := 0 to GetArrayLength(Values)-1 do begin
    RecoverySnapshotValue(Values[I].Key, Values[I].Name);
    RecoveryValues[GetArrayLength(RecoveryValues)-1].DefaultAppValue := True;
  end;
end;

function RecoveryUninstallerName(Name: String): Boolean;
var I: Integer; Extension: String;
begin
  Result := False;
  if Length(Name) <> 12 then Exit;
  if CompareText(Copy(Name, 1, 5), 'unins') <> 0 then Exit;
  for I := 6 to 8 do if (Name[I] < '0') or (Name[I] > '9') then Exit;
  Extension := LowerCase(Copy(Name, 9, 4));
  Result := (Extension = '.exe') or (Extension = '.dat') or (Extension = '.msg');
end;

procedure RecoveryError(Message: String);
begin
  Log('Recovery error: ' + Message);
  if RecoveryErrors <> '' then RecoveryErrors := RecoveryErrors + #13#10;
  RecoveryErrors := RecoveryErrors + Message;
end;

function RecoveryRegularPath(Path: String): Boolean;
var Item: TFindRec; Parent: String;
begin
  Result := False;
  if (CompareText(Path, RecoveryApp) <> 0) and
    (Pos(LowerCase(AddBackslash(RecoveryApp)), LowerCase(Path)) <> 1) then Exit;
  while Path <> '' do begin
    if FindFirst(Path, Item) then begin
      try
        if (Item.Attributes and $400) <> 0 then Exit;
      finally FindClose(Item); end;
    end;
    Parent := ExtractFileDir(Path);
    if Parent = Path then Break;
    Path := Parent;
  end;
  Result := True;
end;

function RecoveryCheck(Path: String): Boolean;
begin
  Result := RecoveryRegularPath(Path);
  if not Result then RecoveryError('Refused changed/reparse path: ' + Path);
end;

function RecoveryShortcutCheck(Index: Integer): Boolean;
begin
  Result := (RecoveryShortcuts[Index].Path = ProductShortcut(RecoveryShortcuts[Index].Legacy)) and
    MigrationRegularPath(RecoveryShortcuts[Index].Path) and
    RecoveryCheck(RecoveryDirectory + '\shortcuts\' + RecoveryShortcuts[Index].BackupName);
  if not Result then RecoveryError('Refused changed shortcut path: ' + RecoveryShortcuts[Index].Path);
end;

procedure RecoverySnapshotShortcut(Legacy: Boolean);
var I: Integer; Path, Backup: String;
begin
  Path := ProductShortcut(Legacy);
  if Legacy and not LegacyShortcutOwned then Exit;
  if not MigrationRegularPath(Path) then RaiseException('Shortcut path changed before snapshot.');
  I := GetArrayLength(RecoveryShortcuts); SetArrayLength(RecoveryShortcuts, I+1);
  RecoveryShortcuts[I].Path := Path; RecoveryShortcuts[I].Legacy := Legacy;
  RecoveryShortcuts[I].BackupName := IntToStr(I) + '.lnk';
  RecoveryShortcuts[I].Existed := FileExists(Path);
  if not RecoveryShortcuts[I].Existed then Exit;
  if not ShortcutOwned(Path, Legacy) then RaiseException('Shortcut ownership changed before snapshot.');
  Backup := RecoveryDirectory + '\shortcuts\' + RecoveryShortcuts[I].BackupName;
  if not RecoveryShortcutCheck(I) or not ForceDirectories(ExtractFileDir(Backup)) or
     not CopyFile(Path, Backup, True) then RaiseException('Cannot snapshot the product shortcut.');
  RecoveryShortcuts[I].OriginalHash := GetSHA256OfFile(Path);
  if GetSHA256OfFile(Backup) <> RecoveryShortcuts[I].OriginalHash then
    RaiseException('Shortcut snapshot hash mismatch.');
end;

function RecoveryRestoreShortcuts: Boolean;
var I: Integer; Path, Backup: String;
begin
  Result := True;
  for I := 0 to GetArrayLength(RecoveryShortcuts)-1 do begin
    Path := RecoveryShortcuts[I].Path;
    Backup := RecoveryDirectory + '\shortcuts\' + RecoveryShortcuts[I].BackupName;
    try
      if not RecoveryShortcutCheck(I) then RaiseException('Shortcut path changed before recovery.');
      if FileExists(Path) then begin
        if RecoveryShortcuts[I].Existed and
           (GetSHA256OfFile(Path) = RecoveryShortcuts[I].OriginalHash) then Continue;
        // A foreign shortcut that appeared after snapshot must survive rollback.
        if RecoveryShortcuts[I].Legacy or not ShortcutOwned(Path, False) then
          RaiseException('Changed shortcut retained: ' + Path);
      end;
      if RecoveryShortcuts[I].Existed then begin
        if not FileExists(Backup) or
           (GetSHA256OfFile(Backup) <> RecoveryShortcuts[I].OriginalHash) then
          RaiseException('Shortcut recovery backup hash differs.');
        if not ForceDirectories(ExtractFileDir(Path)) or not CopyFile(Backup, Path, False) or
           (GetSHA256OfFile(Path) <> RecoveryShortcuts[I].OriginalHash) then
          RaiseException('Cannot restore product shortcut: ' + Path);
      end else if FileExists(Path) then begin
        if not DeleteFile(Path) then RaiseException('Cannot remove newly created product shortcut: ' + Path);
        RemoveDir(ExtractFileDir(Path));
      end;
    except Result := False; RecoveryError(GetExceptionMessage); end;
  end;
end;

function RecoveryPreflight: Boolean;
var I: Integer;
begin
  Result := RecoveryCheck(RecoveryApp);
  if not RecoveryCheck(RecoveryDirectory) then Result := False;
  if not RecoveryCheck(RecoveryDirectory + '\PRODUCT-OWNER.txt') then Result := False;
  if not RecoveryCheck(RecoveryDirectory + '\snapshot.json') then Result := False;
  for I := 0 to GetArrayLength(RecoveryShortcuts)-1 do
    if not RecoveryShortcutCheck(I) then Result := False;
  for I := 0 to GetArrayLength(RecoveryFiles)-1 do begin
    if not RecoveryCheck(AddBackslash(RecoveryApp) + RecoveryFiles[I].Relative) then Result := False;
    if not RecoveryCheck(RecoveryDirectory + '\files\' + RecoveryFiles[I].Relative) then Result := False;
  end;
end;

function RecoveryDelete(Path: String): Boolean;
begin
  Result := RecoveryCheck(Path);
  if Result and FileExists(Path) then begin
    Result := DeleteFile(Path);
    if not Result then RecoveryError('Cannot remove file: ' + Path);
  end;
end;

procedure RecoveryPrune(Directory: String);
begin
  while (Length(Directory) > Length(RecoveryDirectory)) and
    (Pos(AddBackslash(RecoveryDirectory), AddBackslash(Directory)) = 1) do begin
    if not RecoveryCheck(Directory) or not RemoveDir(Directory) then Exit;
    Directory := ExtractFileDir(Directory);
  end;
end;

function RecoveryCleanup: Boolean;
var I: Integer; Path: String; Marker: AnsiString;
begin
  Result := False;
  if not RecoveryPreflight then Exit;
  if not LoadStringFromFile(RecoveryDirectory + '\PRODUCT-OWNER.txt', Marker) or
    (Marker <> OwnerId) then Exit;
  for I := 0 to GetArrayLength(RecoveryFiles)-1 do begin
    Path := RecoveryDirectory + '\files\' + RecoveryFiles[I].Relative;
    if not RecoveryDelete(Path) then Exit;
    RecoveryPrune(ExtractFileDir(Path));
  end;
  if not RecoveryCheck(RecoveryDirectory + '\files') then Exit;
  RemoveDir(RecoveryDirectory + '\files');
  for I := 0 to GetArrayLength(RecoveryShortcuts)-1 do
    if not RecoveryDelete(RecoveryDirectory + '\shortcuts\' + RecoveryShortcuts[I].BackupName) then Exit;
  if not RecoveryCheck(RecoveryDirectory + '\shortcuts') then Exit;
  RemoveDir(RecoveryDirectory + '\shortcuts');
  if not RecoveryDelete(RecoveryDirectory + '\snapshot.json') then Exit;
  if not RecoveryDelete(RecoveryDirectory + '\PRODUCT-OWNER.txt') then Exit;
  Result := RecoveryCheck(RecoveryDirectory) and RemoveDir(RecoveryDirectory);
end;

function RecoveryBegin: String;
var I: Integer; Path, Backup: String; Names, Lines: TArrayOfString; Item: TFindRec;
begin
  Result := '';
  if RecoveryReady then Exit;
  RecoveryErrors := '';
  RecoveryApp := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));
  RecoveryDirectory := RecoveryApp + '\.light-image-upgrade-recovery';
  if FileOrDirExists(RecoveryDirectory) then begin
    Result := 'Existing recovery data was retained. Resolve it before another install: ' + RecoveryDirectory; Exit;
  end;
  SetArrayLength(RecoveryFiles, 0); SetArrayLength(RecoveryValues, 0);
  SetArrayLength(RecoveryShortcuts, 0); SetArrayLength(DefaultAppWrites, 0);
  RecoveryPaths := TStringList.Create;
  try
    PopulateRecoveryPaths(RecoveryPaths);
    // Explicit old/new path union, never populated from an installed manifest.
    if RecoveryPaths.IndexOf(LegacyExeName) < 0 then RecoveryPaths.Add(LegacyExeName);
    if RecoveryPaths.IndexOf(LegacyWorkerName) < 0 then RecoveryPaths.Add(LegacyWorkerName);
    if FindFirst(RecoveryApp + '\unins*', Item) then begin
      try
        repeat
          if RecoveryUninstallerName(Item.Name) and (RecoveryPaths.IndexOf(Item.Name) < 0) then
            RecoveryPaths.Add(Item.Name);
        until not FindNext(Item);
      finally FindClose(Item); end;
    end;
    if RecoveryPaths.Count > 5000 then RaiseException('Recovery file list exceeds bounds.');
    RecoveryUninstallExisted := RegKeyExists(HKCU, UninstallKey);
    if RecoveryUninstallExisted then begin
      // Inno recreates this dedicated key; refuse subtrees we cannot snapshot.
      if not RegGetSubkeyNames(HKCU, UninstallKey, Names) or (GetArrayLength(Names) <> 0) then
        RaiseException('Unexpected uninstall registration subtree; no payload was changed.');
      if not RegGetValueNames(HKCU, UninstallKey, Names) then RaiseException('Cannot read uninstall snapshot.');
      if GetArrayLength(Names) > 128 then RaiseException('Uninstall value list exceeds bounds.');
      for I := 0 to GetArrayLength(Names)-1 do RecoverySnapshotValue(UninstallKey, Names[I]);
    end;
    RecoverySnapshotMenu('.jpg'); RecoverySnapshotMenu('.jpeg'); RecoverySnapshotMenu('.png');
    RecoverySnapshotDefaultApps;
    if not RecoveryRegularPath(RecoveryApp) or not RecoveryRegularPath(RecoveryDirectory) then
      RaiseException('Installation/recovery directory changed before backup.');
    if not ForceDirectories(RecoveryDirectory + '\files') or
      not SaveStringToFile(RecoveryDirectory + '\PRODUCT-OWNER.txt', OwnerId, False) then
      RaiseException('Cannot create owned recovery data. No payload was changed.');
    RecoverySnapshotShortcut(False); RecoverySnapshotShortcut(True);
    for I := 0 to RecoveryPaths.Count-1 do begin
      SetArrayLength(RecoveryFiles, I+1);
      RecoveryFiles[I].Relative := RecoveryPaths[I];
      Path := AddBackslash(RecoveryApp) + RecoveryPaths[I];
      if not RecoveryRegularPath(Path) or DirExists(Path) then
        RaiseException('Non-regular payload path cannot be upgraded.');
      RecoveryFiles[I].Existed := FileExists(Path);
      if RecoveryFiles[I].Existed then begin
        Backup := RecoveryDirectory + '\files\' + RecoveryPaths[I];
        if not RecoveryRegularPath(Backup) or
          not ForceDirectories(ExtractFileDir(Backup)) or not CopyFile(Path, Backup, True) then
          RaiseException('Cannot back up existing payload. No payload was changed.');
        RecoveryFiles[I].OriginalHash := GetSHA256OfFile(Path);
        if GetSHA256OfFile(Backup) <> RecoveryFiles[I].OriginalHash then
          RaiseException('Recovery backup verification failed.');
      end;
    end;
    SetArrayLength(Lines, 1);
    Lines[0] := '{"product_id":' + RecoveryJson(OwnerId) + ',"install_path":' + RecoveryJson(RecoveryApp) + ',"files":[';
    for I := 0 to GetArrayLength(RecoveryFiles)-1 do begin
      if I > 0 then Lines[0] := Lines[0] + ',';
      Lines[0] := Lines[0] + '{"path":' + RecoveryJson(RecoveryFiles[I].Relative) +
        ',"existed":' + IntToStr(Ord(RecoveryFiles[I].Existed)) + ',"sha256":' + RecoveryJson(RecoveryFiles[I].OriginalHash) + '}';
    end;
    Lines[0] := Lines[0] + '],"shortcuts":[';
    for I := 0 to GetArrayLength(RecoveryShortcuts)-1 do begin
      if I > 0 then Lines[0] := Lines[0] + ',';
      Lines[0] := Lines[0] + '{"path":' + RecoveryJson(RecoveryShortcuts[I].Path) +
        ',"backup":' + RecoveryJson(RecoveryShortcuts[I].BackupName) +
        ',"existed":' + IntToStr(Ord(RecoveryShortcuts[I].Existed)) +
        ',"sha256":' + RecoveryJson(RecoveryShortcuts[I].OriginalHash) + '}';
    end;
    Lines[0] := Lines[0] + '],"registry":[';
    for I := 0 to GetArrayLength(RecoveryValues)-1 do begin
      if I > 0 then Lines[0] := Lines[0] + ',';
      Lines[0] := Lines[0] + '{"key":' + RecoveryJson(RecoveryValues[I].Key) + ',"name":' + RecoveryJson(RecoveryValues[I].Name) +
        ',"existed":' + IntToStr(Ord(RecoveryValues[I].Existed)) + ',"type":' + IntToStr(RecoveryValues[I].Kind) +
        ',"text":' + RecoveryJson(RecoveryValues[I].Text) + ',"dword":' + IntToStr(Int64(RecoveryValues[I].Number)) + '}';
    end;
    Lines[0] := Lines[0] + ']}';
    if not SaveStringsToUTF8File(RecoveryDirectory + '\snapshot.json', Lines, False) then
      RaiseException('Cannot persist recovery metadata. No payload was changed.');
    RecoveryReady := True;
    Log('gloamere-image verified recovery snapshot ready before payload changes.');
  except
    Result := GetExceptionMessage;
    if DirExists(RecoveryDirectory) and not RecoveryCleanup then
      Result := Result + ' Retained data: ' + RecoveryDirectory;
  end;
  RecoveryPaths.Free;
end;

function RecoveryRestore: Boolean;
var I, WriteIndex: Integer; Path, Backup, CurrentUninstall: String; Names: TArrayOfString; Okay: Boolean;
begin
  Result := False;
  if not RecoveryPreflight then Exit;
  Result := True;
  for I := 0 to GetArrayLength(RecoveryFiles)-1 do begin
    Path := AddBackslash(RecoveryApp) + RecoveryFiles[I].Relative;
    Backup := RecoveryDirectory + '\files\' + RecoveryFiles[I].Relative;
    try
      if not RecoveryCheck(Path) or not RecoveryCheck(Backup) then
        RaiseException('Path changed before restoration: ' + Path);
      if RecoveryFiles[I].Existed then begin
        if not FileExists(Path) or (GetSHA256OfFile(Path) <> RecoveryFiles[I].OriginalHash) then begin
          if not FileExists(Backup) or (GetSHA256OfFile(Backup) <> RecoveryFiles[I].OriginalHash) then
            RaiseException('Recovery backup hash differs: ' + RecoveryFiles[I].Relative);
          if not ForceDirectories(ExtractFileDir(Path)) or not CopyFile(Backup, Path, False) then
            RaiseException('Cannot restore: ' + RecoveryFiles[I].Relative);
        end;
        if GetSHA256OfFile(Path) <> RecoveryFiles[I].OriginalHash then RaiseException('Restored hash differs.');
      end else if not RecoveryDelete(Path) then RaiseException('Cannot remove new payload: ' + Path);
    except Result := False; RecoveryError(GetExceptionMessage); end;
  end;
  if not RecoveryRestoreShortcuts then Result := False;
  if not RecoveryUninstallExisted and RegQueryStringValue(HKCU, UninstallKey, 'UninstallString', CurrentUninstall) then begin
    Path := RemoveQuotes(CurrentUninstall);
    if (CompareText(ExtractFileDir(Path), RecoveryApp) = 0) and RecoveryUninstallerName(ExtractFileName(Path)) then begin
      if not RecoveryDelete(Path) then Result := False;
      if not RecoveryDelete(ChangeFileExt(Path, '.dat')) then Result := False;
      if not RecoveryDelete(ChangeFileExt(Path, '.msg')) then Result := False;
    end;
  end;
  if RegKeyExists(HKCU, UninstallKey) then begin
    if not RegGetValueNames(HKCU, UninstallKey, Names) then Result := False
    else for I := 0 to GetArrayLength(Names)-1 do
      if not RegDeleteValue(HKCU, UninstallKey, Names[I]) then Result := False;
  end;
  for I := 0 to GetArrayLength(RecoveryValues)-1 do begin
    if RecoveryValues[I].DefaultAppValue then begin
      WriteIndex := DefaultAppWriteIndex(RecoveryValues[I].Key, RecoveryValues[I].Name);
      // Our code never wrote this value; preserve concurrent changes.
      if WriteIndex < 0 then Continue;
      if DefaultAppWrites[WriteIndex].Removed then begin
        // A deleted legacy discovery/menu value is restored only while still absent.
        if RegValueExists(HKCU, RecoveryValues[I].Key, RecoveryValues[I].Name) then begin
          Result := False;
          RecoveryError('Changed deleted product value retained: ' + RecoveryValues[I].Key + '\' + RecoveryValues[I].Name);
          Continue;
        end;
      end else if not MatchesValue(RecoveryValues[I].Key, RecoveryValues[I].Name, DefaultAppWrites[WriteIndex].Text) then begin
        Result := False;
        RecoveryError('Changed default-app registration retained: ' + RecoveryValues[I].Key + '\' + RecoveryValues[I].Name);
        Continue;
      end;
    end;
    if RecoveryValues[I].Existed then begin
      if RecoveryValues[I].Kind = 1 then
        Okay := RegWriteStringValue(HKCU, RecoveryValues[I].Key, RecoveryValues[I].Name, RecoveryValues[I].Text)
      else Okay := RegWriteDWordValue(HKCU, RecoveryValues[I].Key, RecoveryValues[I].Name, RecoveryValues[I].Number);
    end else begin
      Okay := True;
      if RegValueExists(HKCU, RecoveryValues[I].Key, RecoveryValues[I].Name) then
        Okay := RegDeleteValue(HKCU, RecoveryValues[I].Key, RecoveryValues[I].Name);
    end;
    if not Okay then Result := False;
  end;
  if not RecoveryUninstallExisted then RegDeleteKeyIfEmpty(HKCU, UninstallKey);
  for I := GetArrayLength(RecoveryValues)-1 downto 0 do begin
    // Candidate values can live under a shared parent; never prune those here.
    if not RecoveryValues[I].DefaultAppValue then
      RegDeleteKeyIfEmpty(HKCU, RecoveryValues[I].Key);
  end;
  PruneDefaultAppKeys(DefaultAppWritesWithin(DefaultAppRoot), DefaultAppWritesWithin(DefaultAppProgIdKey));
  // Only fixed product menu leaves, never shared registry parents.
  if DefaultAppWritesWithin(MenuKey('.jpg')) then begin
    RegDeleteKeyIfEmpty(HKCU, MenuKey('.jpg') + '\command'); RegDeleteKeyIfEmpty(HKCU, MenuKey('.jpg'));
  end;
  if DefaultAppWritesWithin(MenuKey('.jpeg')) then begin
    RegDeleteKeyIfEmpty(HKCU, MenuKey('.jpeg') + '\command'); RegDeleteKeyIfEmpty(HKCU, MenuKey('.jpeg'));
  end;
  if DefaultAppWritesWithin(MenuKey('.png')) then begin
    RegDeleteKeyIfEmpty(HKCU, MenuKey('.png') + '\command'); RegDeleteKeyIfEmpty(HKCU, MenuKey('.png'));
  end;
end;

procedure DeinitializeSetup;
var Restored: Boolean;
begin
  if not RecoveryReady then Exit;
  Restored := True;
  if not RecoverySuccess and RecoveryStarted then begin
    Log('gloamere-image explicit recovery begins after failed install.');
    Restored := RecoveryRestore;
    if Restored then Log('gloamere-image explicit recovery restored every payload hash and installer-written registry value.');
  end;
  if Restored and RecoveryCleanup then Exit;
  Log('gloamere-image retained recovery data: ' + RecoveryDirectory + #13#10 + RecoveryErrors);
  SuppressibleMsgBox('Recovery data was retained. Close the app and preserve this directory: ' + RecoveryDirectory + #13#10 + RecoveryErrors, mbError, MB_OK, IDOK);
end;
