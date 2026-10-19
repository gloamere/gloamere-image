[Setup]
AppId={{42C10D7A-3E18-47B6-B721-4F003F09AA81}
AppName=Gloamere native security ABI fixture
AppVersion=0.0.1
DefaultDirName={param:FixtureRoot}
CreateAppDir=yes
Uninstallable=no
PrivilegesRequired=lowest
UsePreviousAppDir=no
UsePreviousLanguage=no
OutputDir={#FixtureOutput}
OutputBaseFilename=gloamere-native-inno-security-fixture
SetupLogging=yes
DisableStartupPrompt=yes

[Code]
const OwnerId='NATIVE-FIXTURE-ONLY'; ExeName='unused-fixture.exe';
#include "..\installer\recovery-types.iss"
function CreateFileW(FileName: String; Access, Share: LongWord;
  Security: Integer; Creation, Flags: LongWord; Template: Integer): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function InstalledExe: String;
begin Result := ExpandConstant('{tmp}\unused-fixture.exe'); end;
#include "..\installer\recovery.iss"

var FixturePassed: Integer; FixtureRoot: String;
procedure FixtureProbeAncestor(Path: String);
var H: THandle; O,G,D,S,SD: UINT_PTR; Code, Bytes, Rev: LongWord; Control: Word;
begin
  H := CreateFileW(Path,$20081,3,0,3,$02200000,0);
  if H=THandle(-1) then begin Log('PROBE open failed: '+Path); Exit; end;
  O:=0; G:=0; D:=0; S:=0; SD:=0;
  try
    Code:=NativeGetSecurityInfo(H,1,5,O,G,D,S,SD);
    Log('PROBE '+Path+' GetSecurityInfo='+IntToStr(Code));
    if (Code=0) and (SD<>0) then begin
      Bytes:=NativeGetDescriptorLength(SD); Control:=0; Rev:=0;
      NativeGetDescriptorControl(SD,Control,Rev);
      Log('PROBE bytes='+IntToStr(Bytes)+' owner-offset='+IntToStr(O-SD)+
        ' dacl-offset='+IntToStr(D-SD)+' control='+IntToStr(Control)+
        ' bounded-owner='+IntToStr(Ord(NativeBoundedSid(O,SD,Bytes)))+
        ' trusted-owner='+IntToStr(Ord(NativeTrustedSid(O,False)))+
        ' equal-TI='+IntToStr(NativeEqualSid(O,NativeTrustedInstallerSid))+
        ' strict='+IntToStr(Ord(NativeStrictProductPath(Path))));
    end;
  finally
    if SD<>0 then NativeLocalFree(SD); CloseHandle(H);
  end;
end;
procedure FixtureAssert(Name: String; Condition: Boolean);
begin
  Log('FIXTURE CASE: ' + Name + ', result=' + IntToStr(Ord(Condition)));
  if not Condition then RaiseException(Name + ': ' + NativeAcquireError + ' ' + NativeLaunchError);
  FixturePassed := FixturePassed+1;
end;
function PrepareToInstall(var NeedsRestart: Boolean): String;
var Code: Integer; Extra: THandle; Explanation, ErrorText: String;
begin
  Result := 'Native ABI fixture intentionally refuses installation.';
  NeedsRestart := False;
  FixturePassed := 0;
  try
    FixtureRoot := ExpandConstant('{param:FixtureRoot|}');
    FixtureAssert('fresh bounded root',
      (CompareText(ExtractFileDir(FixtureRoot), 'C:\Windows\Temp') = 0) and
      (Pos('gloamere-inno-security-', ExtractFileName(FixtureRoot)) = 1) and
      NativeCanonicalLocalPath(FixtureRoot) and not DirExists(FixtureRoot));
    FixtureAssert('actual pointer record token descriptor ABI', InitializeNativeSecurity);
    FixtureProbeAncestor('C:\');
    FixtureProbeAncestor('C:\Windows');
    FixtureProbeAncestor('C:\Windows\Temp');
    FixtureAssert('actual OS parent admission', PinNativeAncestors(ExtractFileDir(FixtureRoot)));
    NativeState := FixtureRoot;
    FixtureAssert('actual explicit-owner protected new directory', EnsureNativeProductDirectory(FixtureRoot, True));
    FixtureAssert('existing private directory audited without reset', EnsureNativeProductDirectory(FixtureRoot, True));
    FixtureAssert('new maintenance lock', TakeNativeLock(FixtureRoot+'\maintenance.lock', NativeMaintenance, True));
    FixtureAssert('new sessions lock', TakeNativeLock(FixtureRoot+'\sessions.lock', NativeSessions, True));
    FixtureAssert('existing empty lock pin', PinExistingNativeLock(FixtureRoot+'\maintenance.lock'));
    Extra := 0;
    FixtureAssert('actual byte-range contention', not TakeNativeLock(FixtureRoot+'\maintenance.lock', Extra, False));
    NativeLocksHeld := True;
    NativeHelper := '{#FixtureStub}';
    FixtureAssert('Exec success helper41', NativeCommand('fixture-exit-41',Code) and (Code=41));
    Explanation := NativeReconcileExitError(Code);
    FixtureAssert('owner code41 diagnosis', Pos('not owned',Explanation)>0);
    FixtureAssert('Exec success helper42', NativeCommand('fixture-exit-42',Code) and (Code=42));
    Explanation := NativeReconcileExitError(Code);
    FixtureAssert('write code42 diagnosis', Pos('unsafe or unsupported write',Explanation)>0);
    FixtureAssert('Exec success generic1', NativeCommand('fixture-exit-1',Code) and (Code=1));
    FixtureAssert('generic1 is not pending-health diagnosis', Pos('health',NativeReconcileExitError(Code))=0);
    NativeHelper := FixtureRoot+'\missing-helper.exe';
    FixtureAssert('Exec false reports launch failure', not NativeCommand('fixture-launch-fail',Code));
    FixtureAssert('launch channel preserves Windows error', (Code<>0) and (Pos('Windows error',NativeLaunchError)>0));
    SaveStringToFile('{#FixtureQA}','{"passed":'+IntToStr(FixturePassed)+',"failed":0,"scope":"actual Inno security/diagnostic ABI only; no installation"}',False);
  except
    ErrorText := GetExceptionMessage;
    Log('FIXTURE FAILURE: '+ErrorText);
    SaveStringToFile('{#FixtureQA}','{"passed":'+IntToStr(FixturePassed)+',"failed":1,"scope":"actual Inno security/diagnostic ABI; see raw log"}',False);
  end;
  ReleaseNativeLocks;
  // Match the production PrepareToInstall stage, where {app} is initialized.
  // Always return an error: stop before installation with no registry/files.
end;
