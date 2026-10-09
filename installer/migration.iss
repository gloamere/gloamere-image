function LegacyFileName(Index: Integer): String;
begin
  Result := LegacyExeName;
  if Index = 1 then Result := LegacyWorkerName;
end;

function ProductShortcut(Legacy: Boolean): String;
begin
  Result := ExpandConstant('{userprograms}\gloamere-image\gloamere-image.lnk');
  if Legacy then Result := ExpandConstant('{userprograms}\Light Image Tool\Light Image Tool.lnk');
end;

function MigrationRegularPath(Path: String): Boolean;
var Item: TFindRec; Parent: String;
begin
  Result := False;
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

function ShortcutCompanionsSafe(Legacy: Boolean): Boolean;
var Stem: String;
begin
  Stem := ChangeFileExt(ProductShortcut(Legacy), '');
  // Inno may delete same-stem .pif/.url or directory shortcuts. Never authorize it.
  Result := not FileOrDirExists(Stem + '.pif') and not FileOrDirExists(Stem + '.url') and
    not FileOrDirExists(Stem);
end;

function ShortcutOwned(Path: String; Legacy: Boolean): Boolean;
var Shell, Link: Variant; Target: String;
begin
  Result := False;
  if (Path <> ProductShortcut(Legacy)) or not FileExists(Path) or
     not MigrationRegularPath(Path) then Exit;
  if Legacy and not LegacyInstallationOwned then Exit;
  Target := InstalledExe;
  if Legacy then Target := LegacyInstalledExe;
  try
    // CreateShortcut only reads an existing file here; never call Save.
    Shell := CreateOleObject('WScript.Shell'); Link := Shell.CreateShortcut(Path);
    Result := (CompareText(Link.TargetPath, Target) = 0) and (Link.Arguments = '');
  except
    Result := False;
  end;
end;

procedure RemoveOwnedCurrentShortcut;
var Path: String;
begin
  Path := ProductShortcut(False);
  if ShortcutOwned(Path, False) then begin
    if DeleteFile(Path) then RemoveDir(ExtractFileDir(Path))
    else Log('Changed or locked product shortcut retained: ' + Path);
  end;
end;

function LegacyManifestHash(Name: String; var Hash: String): Boolean;
var Lines: TArrayOfString; I, J, Found, ManifestSize: Integer; Value, Digits: String;
begin
  Result := False; Hash := ''; Found := 0;
  if (Name <> LegacyExeName) and (Name <> LegacyWorkerName) then Exit;
  // Accept only canonical path/size/hash records produced by the legacy packager.
  // Paths are never returned or followed. A noncanonical/missing record fails closed.
  if not FileSize(ExpandConstant('{app}\MANIFEST.json'), ManifestSize) then Exit;
  if (ManifestSize < 1) or (ManifestSize > 4194304) then Exit;
  if not LoadStringsFromFile(ExpandConstant('{app}\MANIFEST.json'), Lines) then Exit;
  if (GetArrayLength(Lines) < 5) or (GetArrayLength(Lines) > 40000) then Exit;
  for I := 1 to GetArrayLength(Lines)-4 do begin
    if Length(Lines[I]) > 4096 then Exit;
    if Trim(Lines[I]) = '"path": "' + Name + '",' then begin
      Found := Found + 1;
      if Found <> 1 then Exit;
      if Trim(Lines[I-1]) <> '{' then Exit;
      Value := Trim(Lines[I+1]);
      if (Pos('"size_bytes": ', Value) <> 1) or (Copy(Value, Length(Value), 1) <> ',') then Exit;
      Digits := Copy(Value, 15, Length(Value)-15);
      if (Length(Digits) = 0) or (Length(Digits) > 12) then Exit;
      for J := 1 to Length(Digits) do if (Digits[J] < '0') or (Digits[J] > '9') then Exit;
      Value := Trim(Lines[I+2]);
      if (Length(Value) <> 76) or (Copy(Value, 1, 11) <> '"sha256": "') or
         (Copy(Value, 76, 1) <> '"') then Exit;
      Hash := Copy(Value, 12, 64);
      for J := 1 to Length(Hash) do
        if not (((Hash[J] >= '0') and (Hash[J] <= '9')) or
                ((Hash[J] >= 'a') and (Hash[J] <= 'f'))) then Exit;
      Value := Trim(Lines[I+3]);
      if (Value <> '}') and (Value <> '},') then Exit;
    end;
  end;
  Result := Found = 1;
end;

function MigrationPreflight: String;
var I: Integer; Path: String;
begin
  Result := ''; LegacyPayloadPresent := False; LegacyShortcutOwned := False;
  if not MigrationRegularPath(ExpandConstant('{app}')) or
     not MigrationRegularPath(ExpandConstant('{app}\MANIFEST.json')) then begin
    Result := 'Installation or manifest path is a reparse point. Nothing was changed.'; Exit;
  end;
  for I := 0 to 1 do
    if FileExists(ExpandConstant('{app}\') + LegacyFileName(I)) then LegacyPayloadPresent := True;
  if LegacyPayloadPresent then begin
    if not LegacyInstallationOwned then begin
      Result := 'Legacy executables lack a matching installation owner. Preserve this directory and restore the original owned install before retrying.'; Exit;
    end;
    for I := 0 to 1 do begin
      Path := ExpandConstant('{app}\') + LegacyFileName(I);
      if not FileExists(Path) or not MigrationRegularPath(Path) or
         not LegacyManifestHash(LegacyFileName(I), LegacyHashes[I]) then begin
        Result := 'Cannot verify the old executable pair from the installed MANIFEST.json. Nothing was changed. Restore the original paired files and manifest from your verified v0.3 package, then retry.'; Exit;
      end;
      if GetSHA256OfFile(Path) <> LegacyHashes[I] then begin
        Result := 'An old executable differs from its installed manifest. Nothing was changed. Preserve it, restore the verified old pair and manifest, then retry.'; Exit;
      end;
    end;
  end;
  if not MigrationRegularPath(ProductShortcut(False)) or
     not MigrationRegularPath(ProductShortcut(True)) then begin
    Result := 'A product Start Menu path is a reparse point. Nothing was changed.'; Exit;
  end;
  if not ShortcutCompanionsSafe(False) then begin
    Result := 'A gloamere-image Start Menu name is occupied by a URL, PIF or directory shortcut. Preserve or rename it before retrying; nothing was changed.'; Exit;
  end;
  if FileOrDirExists(ProductShortcut(False)) and
     (not MarkerOwned or not ShortcutOwned(ProductShortcut(False), False)) then begin
    Result := 'The gloamere-image Start Menu shortcut belongs to another target. Preserve or rename that shortcut before retrying.'; Exit;
  end;
  LegacyShortcutOwned := ShortcutOwned(ProductShortcut(True), True);
  if FileOrDirExists(ProductShortcut(True)) and not LegacyShortcutOwned then begin
    Result := 'Cannot verify the old product Start Menu shortcut target. Nothing was changed. Preserve or rename that shortcut, then retry; Windows Script Host must allow shortcut inspection.'; Exit;
  end;
end;

procedure MigrateLegacyPayload;
var I: Integer; Path: String;
begin
  if not RecoveryReady then RaiseException('Migration requires a verified recovery snapshot.');
  if not MigrationRegularPath(ProductShortcut(False)) or not ShortcutCompanionsSafe(False) or
     (FileOrDirExists(ProductShortcut(False)) and not ShortcutOwned(ProductShortcut(False), False)) then
    RaiseException('The destination shortcut changed after snapshot.');
  if LegacyPayloadPresent then begin
    if not LegacyInstallationOwned then RaiseException('Legacy installation owner changed.');
    // Verify the entire old pair before deleting either file.
    for I := 0 to 1 do begin
      Path := ExpandConstant('{app}\') + LegacyFileName(I);
      if not MigrationRegularPath(Path) or not FileAvailable(Path) or
         not FileExists(Path) or (GetSHA256OfFile(Path) <> LegacyHashes[I]) then
        RaiseException('Legacy executable changed or is in use after snapshot.');
    end;
    for I := 0 to 1 do begin
      Path := ExpandConstant('{app}\') + LegacyFileName(I);
      if not MigrationRegularPath(Path) or (GetSHA256OfFile(Path) <> LegacyHashes[I]) or
         not DeleteFile(Path) then RaiseException('Cannot remove the verified legacy executable; restoring snapshot.');
    end;
  end;
  if LegacyShortcutOwned then begin
    Path := ProductShortcut(True);
    if not ShortcutOwned(Path, True) then RaiseException('Legacy shortcut changed after snapshot.');
    for I := 0 to GetArrayLength(RecoveryShortcuts)-1 do
      if RecoveryShortcuts[I].Legacy then
        if GetSHA256OfFile(Path) <> RecoveryShortcuts[I].OriginalHash then
          RaiseException('Legacy shortcut bytes changed after snapshot.');
    if not DeleteFile(Path) then
      RaiseException('Cannot remove the owned legacy shortcut; restoring snapshot.');
    // Only the exact old group, and only if empty; never recursive deletion.
    RemoveDir(ExtractFileDir(Path));
  end;
end;
