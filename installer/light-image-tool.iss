; Product-owned per-user integration. No automatic default selection or shell DLL.
#ifndef PayloadDir
  #error Supply /DPayloadDir with the validated installer payload directory
#endif
#ifndef OutputDir
  #error Supply /DOutputDir
#endif
#ifndef PayloadFileList
  #error Supply validated /DPayloadFileList
#endif
#ifndef PayloadRecoveryList
  #error Supply validated /DPayloadRecoveryList
#endif
#define ProductId "9CCB6F43-7782-4F69-9E91-67C6D144737B"
#ifndef ProductVersion
  #define ProductVersion "0.4.0"
#endif
#define ProductExe "gloamere-image.exe"
#define ProductWorker "gloamere-image-worker.exe"
#define MenuVerb "LightImageTool"

[Setup]
AppId={#ProductId}
AppName=gloamere-image
AppVersion={#ProductVersion}
AppVerName=gloamere-image {#ProductVersion} (unsigned test)
DefaultDirName={userpf}\LightImageTool
DefaultGroupName=gloamere-image
UsePreviousGroup=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
UsePreviousAppDir=yes
UsePreviousTasks=yes
DisableProgramGroupPage=yes
SetupIconFile={#PayloadDir}\assets\gloamere-image.ico
UninstallDisplayIcon={app}\assets\gloamere-image.ico
VersionInfoVersion={#ProductVersion}
VersionInfoTextVersion={#ProductVersion}
VersionInfoProductVersion={#ProductVersion}
VersionInfoProductTextVersion={#ProductVersion}
VersionInfoDescription=gloamere-image Setup
VersionInfoProductName=gloamere-image
VersionInfoOriginalFileName=gloamere-image-{#ProductVersion}-Win64-Setup-unsigned.exe
OutputDir={#OutputDir}
OutputBaseFilename=gloamere-image-{#ProductVersion}-Win64-Setup-unsigned
Compression=lzma2
SolidCompression=yes
CloseApplications=no
RestartApplications=no
AllowCancelDuringInstall=yes
ChangesAssociations=yes
SetupLogging=yes
WizardStyle=modern
LicenseFile=..\LICENSE-MIT
Uninstallable=yes
UninstallFilesDir={app}
AppPublisher=gloamere-image contributors

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "compiler:Default.isl,chinese-simplified.isl"

[Tasks]
Name: "explorer"; Description: "{cm:ExplorerTask}"; Flags: unchecked

[CustomMessages]
english.ExplorerTask=Add 'Open with gloamere-image' to image right-click menus (current user)
chinesesimp.ExplorerTask=添加“Open with gloamere-image”图片右键菜单（仅当前用户，不改变默认打开方式）
english.OpenDefaultApps=Open Windows Default Apps settings (choose JPEG / PNG defaults yourself)
chinesesimp.OpenDefaultApps=打开 Windows 默认应用设置（由您自行选择 JPEG / PNG 默认应用）

[Run]
; Opening Settings is a one-time, opt-in action, never a remembered installer task.
Filename: "{code:DefaultAppsSettingsUri}"; Description: "{cm:OpenDefaultApps}"; Flags: postinstall unchecked shellexec skipifsilent nowait

[Files]
; Inno logs each exact file and removes only that installed payload on uninstall.
#include PayloadFileList

[Icons]
Name: "{userprograms}\gloamere-image\gloamere-image"; Filename: "{app}\{#ProductExe}"; IconFilename: "{app}\assets\gloamere-image.ico"; AppUserModelID: "LightImageTool.Image"; Flags: uninsneveruninstall

[Registry]
; Registration is written through guarded tracked code after the full snapshot.

[Code]
const
  OwnerId = '{#ProductId}';
  ExeName = '{#ProductExe}';
  WorkerName = '{#ProductWorker}';
  LegacyExeName = 'light-image-tool.exe';
  LegacyWorkerName = 'light-provenance-worker.exe';
  LegacyProductName = 'Light Image Tool';
  ProductName = 'gloamere-image';
  MarkerName = 'LIGHT-IMAGE-INSTALL.txt';
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#ProductId}_is1';

#include "recovery-types.iss"
#include "default-apps-types.iss"
#include "migration-types.iss"

function CreateFileW(FileName: String; Access, Share: LongWord;
  Security: Integer; Creation, Flags: LongWord; Template: Integer): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

#include "registry-values.iss"

function MenuKey(Ext: String): String;
begin
  Result := 'Software\Classes\SystemFileAssociations\' + Ext + '\shell\{#MenuVerb}';
end;

function InstalledExe: String;
begin
  Result := ExpandConstant('{app}\') + ExeName;
end;

function MenuCommand(Path: String): String;
begin
  Result := '"' + Path + '" --open "%1"';
end;

function MatchesValue(Key, Name, Expected: String): Boolean;
var Value: String; Kind: LongWord;
begin
  Result := False;
  // RegQueryStringValue also accepts REG_EXPAND_SZ; ownership requires REG_SZ.
  if not QueryHkcuValueKind(Key, Name, Kind) then Exit;
  if Kind <> 1 then Exit;
  if not RegQueryStringValue(HKCU, Key, Name, Value) then Exit;
  if Value <> Expected then Exit;
  if not QueryHkcuValueKind(Key, Name, Kind) then Exit;
  Result := Kind = 1;
  // This is a conservative check, not an atomic compare-and-delete operation.
end;

procedure DeleteMatching(Key, Name, Expected: String);
begin
  if MatchesValue(Key, Name, Expected) then RegDeleteValue(HKCU, Key, Name);
end;

function MarkerOwned: Boolean;
var Text: AnsiString;
begin
  Result := LoadStringFromFile(ExpandConstant('{app}\') + MarkerName, Text) and (Text = OwnerId);
end;

function LegacyInstalledExe: String;
begin
  Result := ExpandConstant('{app}\') + LegacyExeName;
end;

function InstalledIcon: String;
begin
  Result := ExpandConstant('{app}\assets\gloamere-image.ico');
end;

function LegacyInstallationOwned: Boolean;
var Path: String;
begin
  Result := MarkerOwned and RegQueryStringValue(HKCU, UninstallKey, 'InstallLocation', Path);
  if Result then Result := MatchesValue(UninstallKey, 'InstallLocation', Path) and
    (CompareText(AddBackslash(Path), AddBackslash(ExpandConstant('{app}'))) = 0);
end;

#include "default-apps.iss"
#include "menus.iss"

function FileAvailable(Path: String): Boolean;
var Handle: THandle;
begin
  Result := True;
  if not FileExists(Path) then Exit;
  Handle := CreateFileW(Path, $40000000, 0, 0, 3, $80, 0);
  Result := Handle <> THandle(-1);
  if Result then CloseHandle(Handle);
end;

function HasEntries(Path: String): Boolean;
var FindRec: TFindRec;
begin
  Result := False;
  if FindFirst(AddBackslash(Path) + '*', FindRec) then begin
    try
      repeat
        if (FindRec.Name <> '.') and (FindRec.Name <> '..') then begin Result := True; Break; end;
      until not FindNext(FindRec);
    finally FindClose(FindRec); end;
  end;
end;

#include "migration.iss"
#include "recovery.iss"

function PrepareToInstall(var NeedsRestart: Boolean): String;
var Path, PreviousPath: String;
begin
  Result := ''; Path := ExpandConstant('{app}');
  if RegKeyExists(HKCU, UninstallKey) then begin
    if not RegQueryStringValue(HKCU, UninstallKey, 'InstallLocation', PreviousPath) or
       not MatchesValue(UninstallKey, 'InstallLocation', PreviousPath) or
       (CompareText(AddBackslash(PreviousPath), AddBackslash(Path)) <> 0) or not MarkerOwned then begin
      Result := 'Upgrade must use the existing owned installation directory. To relocate, uninstall first; images and settings are retained.'; Exit;
    end;
  end;
  if (Length(Path) < 8) or (CompareText(Path, ExpandConstant('{win}')) = 0) or
     (CompareText(Path, ExpandConstant('{sys}')) = 0) then begin
    Result := 'Choose a dedicated application directory.'; Exit;
  end;
  if HasEntries(Path) and not MarkerOwned then begin
    Result := 'This directory contains files not owned by gloamere-image. Choose an empty directory.'; Exit;
  end;
  if not FileAvailable(InstalledExe) or not FileAvailable(AddBackslash(Path) + WorkerName) or
     not FileAvailable(AddBackslash(Path) + LegacyExeName) or
     not FileAvailable(AddBackslash(Path) + LegacyWorkerName) then begin
    Result := 'Close gloamere-image, Light Image Tool and their source-check workers before installing. No application was closed automatically.'; Exit;
  end;
  // Refuse changed old menus even when unchecked: never strand a broken handler.
  if not MenuSafe('.jpg') or not MenuSafe('.jpeg') or not MenuSafe('.png') then begin
    Result := 'A LightImageTool menu key contains foreign or changed values. Nothing was overwritten. Preserve and review the product-specific key before installing.'; Exit;
  end;
  if not DefaultAppsSafe then begin
    Result := 'A gloamere-image default-app registration is partial, foreign or changed. Nothing was overwritten. Preserve and review the product-specific registration before installing.'; Exit;
  end;
  Result := MigrationPreflight;
  if Result <> '' then Exit;
  Result := RecoveryBegin;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then begin
    RecoveryStarted := True;
    // Fatal ssInstall failures trigger explicit recovery after Inno rollback.
    if not MenuSafe('.jpg') or not MenuSafe('.jpeg') or not MenuSafe('.png') then
      RaiseException('Image menu registration changed after preflight.');
    if not DefaultAppsSafe then RaiseException('Candidate registration changed after preflight.');
    MigrateLegacyPayload;
    InstallMenus;
    InstallDefaultApps;
  end;
  if CurStep = ssDone then RecoverySuccess := True;
end;

function InitializeUninstall: Boolean;
var RegisteredPath: String;
begin
  Result := MarkerOwned and FileAvailable(InstalledExe) and
    FileAvailable(ExpandConstant('{app}\') + WorkerName);
  if Result and RegKeyExists(HKCU, UninstallKey) then
    Result := RegQueryStringValue(HKCU, UninstallKey, 'InstallLocation', RegisteredPath) and
      MatchesValue(UninstallKey, 'InstallLocation', RegisteredPath) and
      (CompareText(AddBackslash(RegisteredPath), AddBackslash(ExpandConstant('{app}'))) = 0);
  if Result then begin
    Result := MigrationRegularPath(ExpandConstant('{app}')) and
      ShortcutCompanionsSafe(False) and ShortcutCompanionsSafe(True) and
      MigrationRegularPath(ProductShortcut(False)) and MigrationRegularPath(ProductShortcut(True));
    if FileOrDirExists(ProductShortcut(False)) and not ShortcutOwned(ProductShortcut(False), False) then Result := False;
    if FileOrDirExists(ProductShortcut(True)) and not ShortcutOwned(ProductShortcut(True), True) then Result := False;
  end;
  if not Result then SuppressibleMsgBox('Close the app/worker and verify this is an owned gloamere-image installation. Preserve or rename any changed product Start Menu shortcut before retrying. No files have been removed.', mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then begin
    RemoveOwnedCurrentShortcut;
    RemoveOwnedMenu('.jpg'); RemoveOwnedMenu('.jpeg'); RemoveOwnedMenu('.png');
    RemoveOwnedDefaultApps;
  end;
end;
