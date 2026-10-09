// Each legacy/current Explorer verb is an exact complete REG_SZ profile.
procedure PopulateMenuValues(var Values: TDefaultAppValues; Ext: String; Legacy: Boolean);
var Key, Path, LabelName, Icon: String;
begin
  SetArrayLength(Values, 0); Key := MenuKey(Ext);
  Path := InstalledExe; LabelName := ProductName; Icon := InstalledIcon;
  if Legacy then begin
    Path := LegacyInstalledExe; LabelName := LegacyProductName; Icon := Path;
  end;
  AddDefaultAppValue(Values, Key, 'OwnerId', OwnerId);
  AddDefaultAppValue(Values, Key, 'OwnerPath', Path);
  AddDefaultAppValue(Values, Key, 'MUIVerb', 'Open with ' + LabelName);
  AddDefaultAppValue(Values, Key, 'Icon', '"' + Icon + '",0');
  AddDefaultAppValue(Values, Key, 'MultiSelectModel', 'Single');
  AddDefaultAppValue(Values, Key + '\command', '', MenuCommand(Path));
end;

function MenuProfileMatches(Ext: String; Legacy: Boolean): Boolean;
var Values: TDefaultAppValues;
begin
  Result := False;
  if Legacy and not LegacyInstallationOwned then Exit;
  if not RegKeyExists(HKCU, MenuKey(Ext)) then Exit;
  PopulateMenuValues(Values, Ext, Legacy);
  Result := DefaultAppTreeMatches(MenuKey(Ext), Values);
end;

function MenuSafe(Ext: String): Boolean;
begin
  Result := not RegKeyExists(HKCU, MenuKey(Ext)) or
    MenuProfileMatches(Ext, False) or MenuProfileMatches(Ext, True);
end;

procedure InstallMenu(Ext: String);
var Values: TDefaultAppValues; I: Integer;
begin
  if not MenuSafe(Ext) then RaiseException('Image menu changed during installation.');
  if WizardIsTaskSelected('explorer') then begin
    PopulateMenuValues(Values, Ext, False);
    for I := 0 to GetArrayLength(Values)-1 do begin
      if not RegWriteStringValue(HKCU, Values[I].Key, Values[I].Name, Values[I].Text) then
        RaiseException('Cannot write the owned image menu.');
      AddDefaultAppValue(DefaultAppWrites, Values[I].Key, Values[I].Name, Values[I].Text);
    end;
  end else begin
    if not RegKeyExists(HKCU, MenuKey(Ext)) then Exit;
    PopulateMenuValues(Values, Ext, MenuProfileMatches(Ext, True));
    for I := GetArrayLength(Values)-1 downto 0 do begin
      if not MatchesValue(Values[I].Key, Values[I].Name, Values[I].Text) then
        RaiseException('Image menu value changed during removal.');
      if not RegDeleteValue(HKCU, Values[I].Key, Values[I].Name) then
        RaiseException('Cannot remove the owned image menu.');
      RecordProductDeletion(Values[I].Key, Values[I].Name, Values[I].Text);
    end;
    RegDeleteKeyIfEmpty(HKCU, MenuKey(Ext) + '\command');
    RegDeleteKeyIfEmpty(HKCU, MenuKey(Ext));
  end;
end;

procedure InstallMenus;
begin
  InstallMenu('.jpg'); InstallMenu('.jpeg'); InstallMenu('.png');
end;

procedure RemoveOwnedMenu(Ext: String);
var Values: TDefaultAppValues; I: Integer; Key: String;
begin
  Key := MenuKey(Ext);
  // Uninstall preserves changed/foreign values and removes exact owned values only.
  if not MatchesValue(Key, 'OwnerId', OwnerId) or
     not MatchesValue(Key, 'OwnerPath', InstalledExe) or
     not MatchesValue(Key + '\command', '', MenuCommand(InstalledExe)) then Exit;
  PopulateMenuValues(Values, Ext, False);
  for I := GetArrayLength(Values)-1 downto 0 do
    DeleteMatching(Values[I].Key, Values[I].Name, Values[I].Text);
  RegDeleteKeyIfEmpty(HKCU, Key + '\command');
  RegDeleteKeyIfEmpty(HKCU, Key);
end;
