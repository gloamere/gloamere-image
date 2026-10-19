// Registers a candidate handler only. Windows owns the user's default choice.
// Never write extension defaults, UserChoice, policies, HKLM or HKCR.
function DefaultAppRoot: String;
begin
  Result := 'Software\LightImageTool\DefaultApps';
end;

function DefaultAppProgIdKey: String;
begin
  Result := 'Software\Classes\LightImageTool.Image';
end;

procedure AddDefaultAppValue(var Values: TDefaultAppValues; Key, Name, Text: String);
var I: Integer;
begin
  I := GetArrayLength(Values);
  SetArrayLength(Values, I+1);
  Values[I].Key := Key; Values[I].Name := Name; Values[I].Text := Text;
  Values[I].Removed := False;
end;

procedure PopulateDefaultAppProfile(var Values: TDefaultAppValues; Profile: TDefaultAppProfile);
var Root, Capabilities, ProgId, Path, Icon, LabelName, Description, ImageLabel: String;
begin
  SetArrayLength(Values, 0);
  Root := DefaultAppRoot; Capabilities := Root + '\Capabilities';
  ProgId := DefaultAppProgIdKey; Path := InstalledExe;
  LabelName := ProductName; Icon := '"' + InstalledIcon + '",0';
  if Profile = dapLegacy then begin
    Path := LegacyInstalledExe; LabelName := LegacyProductName;
    Icon := '"' + Path + '",0';
  end;
  Description := 'Local JPEG and PNG image viewer and editor.';
  ImageLabel := ' JPEG / PNG image';
  if Profile = dapV05 then begin
    Description := 'Local JPEG, PNG, GIF and WebP viewer with static-copy editing.';
    ImageLabel := ' JPEG / PNG / GIF / WebP image';
  end;
  AddDefaultAppValue(Values, Root, 'OwnerId', OwnerId);
  AddDefaultAppValue(Values, Root, 'OwnerPath', Path);
  AddDefaultAppValue(Values, Capabilities, 'ApplicationName', LabelName);
  AddDefaultAppValue(Values, Capabilities, 'ApplicationDescription', Description);
  AddDefaultAppValue(Values, Capabilities, 'ApplicationIcon', Icon);
  AddDefaultAppValue(Values, Capabilities + '\FileAssociations', '.jpg', 'LightImageTool.Image');
  AddDefaultAppValue(Values, Capabilities + '\FileAssociations', '.jpeg', 'LightImageTool.Image');
  AddDefaultAppValue(Values, Capabilities + '\FileAssociations', '.png', 'LightImageTool.Image');
  if Profile = dapV05 then begin
    AddDefaultAppValue(Values, Capabilities + '\FileAssociations', '.gif', 'LightImageTool.Image');
    AddDefaultAppValue(Values, Capabilities + '\FileAssociations', '.webp', 'LightImageTool.Image');
  end;
  AddDefaultAppValue(Values, ProgId, 'OwnerId', OwnerId);
  AddDefaultAppValue(Values, ProgId, 'OwnerPath', Path);
  AddDefaultAppValue(Values, ProgId, '', LabelName + ImageLabel);
  AddDefaultAppValue(Values, ProgId + '\DefaultIcon', '', Icon);
  AddDefaultAppValue(Values, ProgId + '\shell\open\command', '', MenuCommand(Path));
  // Publish discovery last, only after the candidate handler is complete.
  AddDefaultAppValue(Values, 'Software\RegisteredApplications', LabelName, Capabilities);
end;

procedure PopulateDefaultAppValues(var Values: TDefaultAppValues);
begin
  PopulateDefaultAppProfile(Values, dapV05);
end;

procedure RecordProductDeletion(Key, Name, Text: String);
var I: Integer;
begin
  AddDefaultAppValue(DefaultAppWrites, Key, Name, Text);
  I := GetArrayLength(DefaultAppWrites)-1;
  DefaultAppWrites[I].Removed := True;
end;

function DefaultAppKeyWithin(Key, Root: String): Boolean;
begin
  Result := (CompareText(Key, Root) = 0) or
    (Pos(LowerCase(Root + '\'), LowerCase(Key)) = 1);
end;

function DefaultAppTreeMatches(Key: String; Values: TDefaultAppValues): Boolean;
var Names: TArrayOfString; I, J, ExpectedCount: Integer; Child: String; Known: Boolean;
begin
  Result := False; ExpectedCount := 0;
  if not RegGetValueNames(HKCU, Key, Names) then Exit;
  for I := 0 to GetArrayLength(Values)-1 do begin
    if CompareText(Values[I].Key, Key) = 0 then begin
      ExpectedCount := ExpectedCount + 1;
      if not MatchesValue(Key, Values[I].Name, Values[I].Text) then Exit;
    end;
    if DefaultAppKeyWithin(Values[I].Key, Key) and
      not RegKeyExists(HKCU, Values[I].Key) then Exit;
  end;
  if GetArrayLength(Names) <> ExpectedCount then Exit;
  if not RegGetSubkeyNames(HKCU, Key, Names) then Exit;
  for I := 0 to GetArrayLength(Names)-1 do begin
    Child := Key + '\' + Names[I]; Known := False;
    for J := 0 to GetArrayLength(Values)-1 do
      if DefaultAppKeyWithin(Values[J].Key, Child) then Known := True;
    if not Known then Exit;
    if not DefaultAppTreeMatches(Child, Values) then Exit;
  end;
  Result := True;
end;

function DefaultAppProfileMatches(Profile: TDefaultAppProfile): Boolean;
var Values: TDefaultAppValues; LabelName, OtherName: String;
begin
  Result := False;
  LabelName := ProductName; OtherName := LegacyProductName;
  if (Profile <> dapV05) and not LegacyInstallationOwned then Exit;
  if Profile = dapLegacy then begin
    LabelName := LegacyProductName; OtherName := ProductName;
  end;
  PopulateDefaultAppProfile(Values, Profile);
  if not RegKeyExists(HKCU, DefaultAppRoot) or not RegKeyExists(HKCU, DefaultAppProgIdKey) then Exit;
  if not DefaultAppTreeMatches(DefaultAppRoot, Values) then Exit;
  if not DefaultAppTreeMatches(DefaultAppProgIdKey, Values) then Exit;
  if RegValueExists(HKCU, 'Software\RegisteredApplications', OtherName) then Exit;
  Result := MatchesValue('Software\RegisteredApplications', LabelName,
    DefaultAppRoot + '\Capabilities');
end;

function DefaultAppsSafe: Boolean;
begin
  Result := not RegKeyExists(HKCU, DefaultAppRoot) and
    not RegKeyExists(HKCU, DefaultAppProgIdKey) and
    not RegValueExists(HKCU, 'Software\RegisteredApplications', ProductName) and
    not RegValueExists(HKCU, 'Software\RegisteredApplications', LegacyProductName);
  if Result then Exit;
  Result := DefaultAppProfileMatches(dapV05) or
    DefaultAppProfileMatches(dapV04) or DefaultAppProfileMatches(dapLegacy);
end;

procedure InstallDefaultApps;
var Values: TDefaultAppValues; I: Integer; LegacyDiscovery: Boolean;
begin
  // Recheck after snapshot preparation, before the first registration write.
  if not DefaultAppsSafe then
    RaiseException('Default-app registration changed during installation; restoring the installation snapshot.');
  LegacyDiscovery := DefaultAppProfileMatches(dapLegacy);
  PopulateDefaultAppValues(Values);
  for I := 0 to GetArrayLength(Values)-1 do begin
    if not RegWriteStringValue(HKCU, Values[I].Key, Values[I].Name, Values[I].Text) then
      RaiseException('Cannot register the JPEG / PNG / GIF / WebP candidate handler; restoring the installation snapshot.');
    AddDefaultAppValue(DefaultAppWrites, Values[I].Key, Values[I].Name, Values[I].Text);
  end;
  if LegacyDiscovery then begin
    if not MatchesValue('Software\RegisteredApplications', LegacyProductName, DefaultAppRoot + '\Capabilities') then
      RaiseException('Legacy discovery value changed during migration.');
    if not RegDeleteValue(HKCU, 'Software\RegisteredApplications', LegacyProductName) then
      RaiseException('Cannot remove the owned legacy discovery value.');
    RecordProductDeletion('Software\RegisteredApplications', LegacyProductName, DefaultAppRoot + '\Capabilities');
  end;
end;

function DefaultAppWriteIndex(Key, Name: String): Integer;
var I: Integer;
begin
  Result := -1;
  for I := 0 to GetArrayLength(DefaultAppWrites)-1 do
    if (CompareText(Key, DefaultAppWrites[I].Key) = 0) and
       (CompareText(Name, DefaultAppWrites[I].Name) = 0) then begin
      Result := I; Exit;
    end;
end;

function DefaultAppWritesWithin(Root: String): Boolean;
var I: Integer;
begin
  Result := False;
  for I := 0 to GetArrayLength(DefaultAppWrites)-1 do
    if DefaultAppKeyWithin(DefaultAppWrites[I].Key, Root) then begin
      Result := True; Exit;
    end;
end;

procedure PruneDefaultAppKeys(RootOwned, ProgIdOwned: Boolean);
begin
  // Never recursively delete a tree or remove shared registry parents.
  if RootOwned then begin
    RegDeleteKeyIfEmpty(HKCU, DefaultAppRoot + '\Capabilities\FileAssociations');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppRoot + '\Capabilities');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppRoot);
  end;
  if ProgIdOwned then begin
    RegDeleteKeyIfEmpty(HKCU, DefaultAppProgIdKey + '\shell\open\command');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppProgIdKey + '\shell\open');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppProgIdKey + '\shell');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppProgIdKey + '\DefaultIcon');
    RegDeleteKeyIfEmpty(HKCU, DefaultAppProgIdKey);
  end;
end;

procedure RemoveOwnedDefaultApps;
var Values: TDefaultAppValues; I: Integer; RootOwned, ProgIdOwned: Boolean;
begin
  RootOwned := MatchesValue(DefaultAppRoot, 'OwnerId', OwnerId) and
    MatchesValue(DefaultAppRoot, 'OwnerPath', InstalledExe);
  ProgIdOwned := MatchesValue(DefaultAppProgIdKey, 'OwnerId', OwnerId) and
    MatchesValue(DefaultAppProgIdKey, 'OwnerPath', InstalledExe) and
    MatchesValue(DefaultAppProgIdKey + '\shell\open\command', '', MenuCommand(InstalledExe));
  PopulateDefaultAppValues(Values);
  // Discovery first, ownership markers last; changed and foreign values survive.
  for I := GetArrayLength(Values)-1 downto 0 do begin
    if (RootOwned and (DefaultAppKeyWithin(Values[I].Key, DefaultAppRoot) or
       (CompareText(Values[I].Key, 'Software\RegisteredApplications') = 0))) or
       (ProgIdOwned and DefaultAppKeyWithin(Values[I].Key, DefaultAppProgIdKey)) then
      DeleteMatching(Values[I].Key, Values[I].Name, Values[I].Text);
  end;
  PruneDefaultAppKeys(RootOwned, ProgIdOwned);
end;

function DefaultAppsSettingsUri(Param: String): String;
var Version: TWindowsVersion;
begin
  Result := 'ms-settings:defaultapps';
  GetWindowsVersionEx(Version);
  // 21H2/22H2 need specific cumulative updates. Avoid guessing their patch state.
  // 23H2 (22631) and later Windows 11 client builds support the documented link.
  if (Version.Major = 10) and (Version.Minor = 0) and
     (Version.ProductType = VER_NT_WORKSTATION) and (Version.Build >= 22631) then
    Result := Result + '?registeredAppUser=gloamere-image';
end;
