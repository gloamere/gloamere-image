// The persisted Rust journal is the single recovery owner. Inno's automatic
// rollback is not a power-loss/health-check transaction and is never sufficient.
// Security ABI: pinned Inno Setup 6.7.3 is a 32-bit process on x64 Windows.
// Pascal Script records are packed; fail closed if these layouts ever change.
type
  TNativeSecurityAttributes = record
    Length: LongWord;
    Descriptor: UINT_PTR;
    InheritHandle: Integer;
  end;
  TNativeFileInfo = record
    Attributes: LongWord;
    Created, Accessed, Written: TFileTime;
    VolumeSerial, SizeHigh, SizeLow, Links, IndexHigh, IndexLow: LongWord;
  end;
  TNativeAclSize = record
    AceCount, BytesInUse, BytesFree: LongWord;
  end;
  TNativeAcePrefix = record
    AceType, Flags: Byte;
    Size: Word;
    Mask: LongWord;
  end;
  TNativeAceHeader = record
    AceType, Flags: Byte;
    Size: Word;
  end;
const
  NativeProductWriteMask = $500D0156;
  NativeAncestorWriteMask = $500D0150;
  NativeLauncherMaxBytes = 536870912;
var
  NativeHelper, NativePlan, NativeState, NativeLaunchError, NativeAcquireError, NativeVerifiedLauncherHash: String;
  NativeMaintenance, NativeSessions, NativeStampPin, NativeLauncherPin: THandle;
  NativePathPins: array of THandle;
  NativeUserBuffer, NativeUserSid, NativeSystemSid, NativeAdminsSid, NativeTrustedInstallerSid: UINT_PTR;
  NativeNewDescriptor, NativeNewAcl: UINT_PTR;
  NativeNewAttributes: TNativeSecurityAttributes;
  NativeLocksHeld, NativeBootstrap, NativePendingHealth, NativeFailureDetected: Boolean;

function NativeLockFile(Handle: THandle; OffsetLow, OffsetHigh, CountLow, CountHigh: LongWord): Integer;
  external 'LockFile@kernel32.dll stdcall';
function NativeGetCurrentProcessId: LongWord;
  external 'GetCurrentProcessId@kernel32.dll stdcall';
function NativeGetCurrentProcess: THandle;
  external 'GetCurrentProcess@kernel32.dll stdcall';
function NativeLocalAlloc(Flags: LongWord; Bytes: UINT_PTR): UINT_PTR;
  external 'LocalAlloc@kernel32.dll stdcall';
function NativeLocalFree(Address: UINT_PTR): UINT_PTR;
  external 'LocalFree@kernel32.dll stdcall';
procedure NativeCopyPointer(var Destination: UINT_PTR; Source, Bytes: UINT_PTR);
  external 'RtlMoveMemory@kernel32.dll stdcall';
procedure NativeCopyAcePrefix(var Destination: TNativeAcePrefix; Source, Bytes: UINT_PTR);
  external 'RtlMoveMemory@kernel32.dll stdcall';
procedure NativeCopyAceHeader(var Destination: TNativeAceHeader; Source, Bytes: UINT_PTR);
  external 'RtlMoveMemory@kernel32.dll stdcall';
function NativeOpenProcessToken(Process: THandle; Access: LongWord; var Token: THandle): Integer;
  external 'OpenProcessToken@advapi32.dll stdcall';
function NativeGetTokenInformation(Token: THandle; Kind: Integer; Buffer: UINT_PTR;
  Size: LongWord; var Needed: LongWord): Integer;
  external 'GetTokenInformation@advapi32.dll stdcall';
function NativeConvertStringSid(Text: String; var Sid: UINT_PTR): Integer;
  external 'ConvertStringSidToSidW@advapi32.dll stdcall';
function NativeIsValidSid(Sid: UINT_PTR): Integer;
  external 'IsValidSid@advapi32.dll stdcall';
function NativeGetLengthSid(Sid: UINT_PTR): LongWord;
  external 'GetLengthSid@advapi32.dll stdcall';
function NativeEqualSid(Left, Right: UINT_PTR): Integer;
  external 'EqualSid@advapi32.dll stdcall';
function NativeGetSecurityInfo(Handle: THandle; ObjectType, Information: LongWord;
  var Owner, Group, Dacl, Sacl, Descriptor: UINT_PTR): LongWord;
  external 'GetSecurityInfo@advapi32.dll stdcall';
function NativeIsValidSecurityDescriptor(Descriptor: UINT_PTR): Integer;
  external 'IsValidSecurityDescriptor@advapi32.dll stdcall';
function NativeGetDescriptorLength(Descriptor: UINT_PTR): LongWord;
  external 'GetSecurityDescriptorLength@advapi32.dll stdcall';
function NativeGetDescriptorControl(Descriptor: UINT_PTR; var Control: Word; var Revision: LongWord): Integer;
  external 'GetSecurityDescriptorControl@advapi32.dll stdcall';
function NativeIsValidAcl(Acl: UINT_PTR): Integer;
  external 'IsValidAcl@advapi32.dll stdcall';
function NativeGetAclInformation(Acl: UINT_PTR; var Info: TNativeAclSize;
  Size, Kind: LongWord): Integer;
  external 'GetAclInformation@advapi32.dll stdcall';
function NativeGetAce(Acl: UINT_PTR; Index: LongWord; var Ace: UINT_PTR): Integer;
  external 'GetAce@advapi32.dll stdcall';
function NativeInitializeAcl(Acl: UINT_PTR; Size, Revision: LongWord): Integer;
  external 'InitializeAcl@advapi32.dll stdcall';
function NativeAddAllowedAce(Acl: UINT_PTR; Revision, Flags, Mask: LongWord; Sid: UINT_PTR): Integer;
  external 'AddAccessAllowedAceEx@advapi32.dll stdcall';
function NativeInitializeDescriptor(Descriptor: UINT_PTR; Revision: LongWord): Integer;
  external 'InitializeSecurityDescriptor@advapi32.dll stdcall';
function NativeSetDescriptorOwner(Descriptor, Owner: UINT_PTR; Defaulted: Integer): Integer;
  external 'SetSecurityDescriptorOwner@advapi32.dll stdcall';
function NativeSetDescriptorDacl(Descriptor: UINT_PTR; Present: Integer;
  Acl: UINT_PTR; Defaulted: Integer): Integer;
  external 'SetSecurityDescriptorDacl@advapi32.dll stdcall';
function NativeSetDescriptorControl(Descriptor: UINT_PTR; Bits, Values: Word): Integer;
  external 'SetSecurityDescriptorControl@advapi32.dll stdcall';
function NativeCreateDirectory(Path: String; var Security: TNativeSecurityAttributes): Integer;
  external 'CreateDirectoryW@kernel32.dll stdcall';
function NativeCreateSecureFile(Path: String; Access, Share: LongWord;
  var Security: TNativeSecurityAttributes; Creation, Flags: LongWord; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function NativeGetFileInformation(Handle: THandle; var Info: TNativeFileInfo): Integer;
  external 'GetFileInformationByHandle@kernel32.dll stdcall';
function NativeGetFileType(Handle: THandle): LongWord;
  external 'GetFileType@kernel32.dll stdcall';

function NativeSecurityExplanation(Code: Integer): String;
begin
  Result := '';
  case Code of
    41: Result := ' A protected product path (installation, updater-state or Start Menu) is not owned by the required Windows principal. Have an administrator review its ownership before retrying; do not bypass the ownership check.';
    42: Result := ' A protected product path (installation, updater-state or Start Menu) has unsafe or unsupported write permissions. Have an administrator review its write permissions before retrying; do not bypass the permissions check.';
  end;
end;

function NativeSecurityRefusal(Path: String; Code: Integer): Boolean;
begin
  Result := False;
  NativeAcquireError := 'Protected path validation failed (diagnostic ' + IntToStr(Code) + ') for "' +
    Path + '".' + NativeSecurityExplanation(Code);
end;

function NativeWindowsRefusal(Action: String; Code: LongWord): Boolean;
begin
  Result := False;
  NativeAcquireError := Action + ' (Windows error ' + IntToStr(Code) + ': ' + SysErrorMessage(Code) + ').';
end;

procedure NativeFreeMemory(var Address: UINT_PTR);
begin
  if Address <> 0 then begin NativeLocalFree(Address); Address := 0; end;
end;

procedure ReleaseNativeBootstrapPins;
begin
  if NativeLauncherPin <> 0 then begin CloseHandle(NativeLauncherPin); NativeLauncherPin := 0; end;
  if NativeStampPin <> 0 then begin CloseHandle(NativeStampPin); NativeStampPin := 0; end;
  NativeVerifiedLauncherHash := '';
end;

procedure ReleaseNativeLocks;
var I: Integer;
begin
  ReleaseNativeBootstrapPins;
  if NativeSessions <> 0 then begin CloseHandle(NativeSessions); NativeSessions := 0; end;
  if NativeMaintenance <> 0 then begin CloseHandle(NativeMaintenance); NativeMaintenance := 0; end;
  for I := GetArrayLength(NativePathPins)-1 downto 0 do CloseHandle(NativePathPins[I]);
  SetArrayLength(NativePathPins, 0);
  NativeFreeMemory(NativeNewDescriptor); NativeFreeMemory(NativeNewAcl);
  NativeFreeMemory(NativeUserBuffer); NativeUserSid := 0;
  NativeFreeMemory(NativeSystemSid); NativeFreeMemory(NativeAdminsSid);
  NativeFreeMemory(NativeTrustedInstallerSid);
  NativeNewAttributes.Descriptor := 0;
  NativeLocksHeld := False;
end;

function InitializeNativeSecurity: Boolean;
var Token: THandle; Needed, BufferSize, SidSize: LongWord;
  FileInfo: TNativeFileInfo; AclInfo: TNativeAclSize; Ace: TNativeAcePrefix; Header: TNativeAceHeader;
begin
  Result := False;
  NativeAcquireError := 'Cannot initialize bounded Windows path-security checks.';
  if (SizeOf(NativeUserSid) <> 4) or (SizeOf(NativeNewAttributes) <> 12) or
     (SizeOf(FileInfo) <> 52) or (SizeOf(AclInfo) <> 12) or
     (SizeOf(Ace) <> 8) or (SizeOf(Header) <> 4) then begin
    NativeAcquireError := 'Unsupported installer security ABI; no product files were changed.'; Exit;
  end;
  Token := 0;
  if NativeOpenProcessToken(NativeGetCurrentProcess, $8, Token) = 0 then begin
    Result := NativeWindowsRefusal('Cannot read the current Windows identity', DLLGetLastError); Exit;
  end;
  try
    Needed := 0;
    NativeGetTokenInformation(Token, 1, 0, 0, Needed);
    if (Needed < 12) or (Needed > 65536) then Exit;
    BufferSize := Needed;
    NativeUserBuffer := NativeLocalAlloc($40, BufferSize);
    if NativeUserBuffer = 0 then Exit;
    if NativeGetTokenInformation(Token, 1, NativeUserBuffer, BufferSize, Needed) = 0 then begin
      Result := NativeWindowsRefusal('Cannot read TokenUser', DLLGetLastError); Exit;
    end;
    if (Needed > BufferSize) or (Needed < 12) then Exit;
    NativeCopyPointer(NativeUserSid, NativeUserBuffer, SizeOf(NativeUserSid));
    if (NativeUserSid < NativeUserBuffer) or (NativeUserSid > NativeUserBuffer + Needed - 8) then Exit;
    if NativeIsValidSid(NativeUserSid) = 0 then Exit;
    SidSize := NativeGetLengthSid(NativeUserSid);
    if (SidSize < 8) or (SidSize > 68) or (SidSize > NativeUserBuffer + Needed - NativeUserSid) then Exit;
    if NativeConvertStringSid('S-1-5-18', NativeSystemSid) = 0 then Exit;
    if NativeConvertStringSid('S-1-5-32-544', NativeAdminsSid) = 0 then Exit;
    if NativeConvertStringSid('S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464', NativeTrustedInstallerSid) = 0 then Exit;
    // Construct an absolute descriptor in memory only. No existing object's
    // owner or ACL is changed. These attributes apply exclusively at creation.
    NativeNewAcl := NativeLocalAlloc($40, 256);
    NativeNewDescriptor := NativeLocalAlloc($40, 64);
    if (NativeNewAcl = 0) or (NativeNewDescriptor = 0) then Exit;
    if NativeInitializeAcl(NativeNewAcl, 256, 2) = 0 then Exit;
    if NativeAddAllowedAce(NativeNewAcl, 2, 3, $1F01FF, NativeUserSid) = 0 then Exit;
    if NativeAddAllowedAce(NativeNewAcl, 2, 3, $1F01FF, NativeSystemSid) = 0 then Exit;
    if NativeAddAllowedAce(NativeNewAcl, 2, 3, $1F01FF, NativeAdminsSid) = 0 then Exit;
    if NativeInitializeDescriptor(NativeNewDescriptor, 1) = 0 then Exit;
    if NativeSetDescriptorOwner(NativeNewDescriptor, NativeUserSid, 0) = 0 then Exit;
    if NativeSetDescriptorDacl(NativeNewDescriptor, 1, NativeNewAcl, 0) = 0 then Exit;
    if NativeSetDescriptorControl(NativeNewDescriptor, $1000, $1000) = 0 then Exit;
    if NativeIsValidSecurityDescriptor(NativeNewDescriptor) = 0 then Exit;
    NativeNewAttributes.Length := SizeOf(NativeNewAttributes);
    NativeNewAttributes.Descriptor := NativeNewDescriptor;
    NativeNewAttributes.InheritHandle := 0;
    Result := True;
  finally
    CloseHandle(Token);
  end;
end;

function NativeTrustedSid(Sid: UINT_PTR; StrictProduct: Boolean): Boolean;
begin
  Result := (NativeEqualSid(Sid, NativeUserSid) <> 0) or
    (NativeEqualSid(Sid, NativeSystemSid) <> 0) or (NativeEqualSid(Sid, NativeAdminsSid) <> 0);
  if not StrictProduct and not Result then Result := NativeEqualSid(Sid, NativeTrustedInstallerSid) <> 0;
end;

function NativeRangeContains(Base: UINT_PTR; Length: LongWord; Address: UINT_PTR; Bytes: LongWord): Boolean;
begin
  Result := False;
  if (Length > 65536) or (Bytes > Length) or (Address < Base) then Exit;
  if Base > High(Base) - Length then Exit;
  Result := Address - Base <= Length - Bytes;
end;

function NativeBoundedSid(Sid, Base: UINT_PTR; Length: LongWord): Boolean;
var Bytes: LongWord;
begin
  Result := False;
  if not NativeRangeContains(Base, Length, Sid, 8) then Exit;
  if NativeIsValidSid(Sid) = 0 then Exit;
  Bytes := NativeGetLengthSid(Sid);
  if (Bytes < 8) or (Bytes > 68) then Exit;
  Result := NativeRangeContains(Base, Length, Sid, Bytes);
end;

function NativeVerifyHandleSecurity(Handle: THandle; Path: String; StrictProduct, RequireProtected: Boolean): Boolean;
var Owner, Group, Dacl, Sacl, Descriptor, AceAddress, Sid: UINT_PTR;
  Code, I, WriteMask, DescriptorSize, Revision: LongWord; Control: Word;
  Acl: TNativeAclSize; Ace: TNativeAcePrefix; Header: TNativeAceHeader;
begin
  Result := False;
  Owner := 0; Group := 0; Dacl := 0; Sacl := 0; Descriptor := 0;
  Code := NativeGetSecurityInfo(Handle, 1, $5, Owner, Group, Dacl, Sacl, Descriptor);
  if Code <> 0 then begin
    if Descriptor <> 0 then NativeLocalFree(Descriptor);
    Result := NativeWindowsRefusal('Cannot inspect security for "' + Path + '"', Code); Exit;
  end;
  try
    if Descriptor = 0 then begin Result := NativeSecurityRefusal(Path, 41); Exit; end;
    if NativeIsValidSecurityDescriptor(Descriptor) = 0 then begin
      Result := NativeSecurityRefusal(Path, 41); Exit;
    end;
    if NativeGetDescriptorControl(Descriptor, Control, Revision) = 0 then begin
      Result := NativeSecurityRefusal(Path, 42); Exit;
    end;
    // GetSecurityInfo's returned owner and DACL must live in its one bounded
    // self-relative allocation, which is retained until the last SID check.
    if (Control and $8000) = 0 then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
    if RequireProtected and ((Control and $1000) = 0) then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
    DescriptorSize := NativeGetDescriptorLength(Descriptor);
    if (DescriptorSize < 20) or (DescriptorSize > 65536) then begin
      Result := NativeSecurityRefusal(Path, 42); Exit;
    end;
    if not NativeBoundedSid(Owner, Descriptor, DescriptorSize) then begin Result := NativeSecurityRefusal(Path, 41); Exit; end;
    if StrictProduct then begin
      if NativeEqualSid(Owner, NativeUserSid) = 0 then begin Result := NativeSecurityRefusal(Path, 41); Exit; end;
    end
    else if not NativeTrustedSid(Owner, False) then begin Result := NativeSecurityRefusal(Path, 41); Exit; end;
    if not NativeRangeContains(Descriptor, DescriptorSize, Dacl, 8) then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
    if NativeIsValidAcl(Dacl) = 0 then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
    if NativeGetAclInformation(Dacl, Acl, SizeOf(Acl), 2) = 0 then begin
      Result := NativeWindowsRefusal('Cannot inspect permissions for "' + Path + '"', DLLGetLastError); Exit;
    end;
    if (Acl.BytesInUse < 8) or (Acl.BytesInUse > 65535) or (Acl.AceCount > 8191) then begin
      Result := NativeSecurityRefusal(Path, 42); Exit;
    end;
    if not NativeRangeContains(Descriptor, DescriptorSize, Dacl, Acl.BytesInUse) then begin
      Result := NativeSecurityRefusal(Path, 42); Exit;
    end;
    WriteMask := NativeAncestorWriteMask;
    if StrictProduct then WriteMask := NativeProductWriteMask;
    if Acl.AceCount > 0 then for I := 0 to Acl.AceCount-1 do begin
      AceAddress := 0;
      if NativeGetAce(Dacl, I, AceAddress) = 0 then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
      if (AceAddress < Dacl + 8) or not NativeRangeContains(Dacl, Acl.BytesInUse, AceAddress, 4) then begin
        Result := NativeSecurityRefusal(Path, 42); Exit;
      end;
      NativeCopyAceHeader(Header, AceAddress, SizeOf(Header));
      if (Header.Size < 4) or not NativeRangeContains(Dacl, Acl.BytesInUse, AceAddress, Header.Size) then begin
        Result := NativeSecurityRefusal(Path, 42); Exit;
      end;
      if (Header.AceType <> 0) and (Header.AceType <> 1) then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
      // INHERIT_ONLY cannot affect an ancestor itself. Product policy remains
      // strict for these entries too, matching the native private-path guard.
      if not StrictProduct and ((Header.Flags and $8) <> 0) then Continue;
      if Header.AceType = 1 then Continue;
      if Header.Size < 16 then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
      NativeCopyAcePrefix(Ace, AceAddress, SizeOf(Ace));
      if (Ace.Mask and WriteMask) = 0 then Continue;
      Sid := AceAddress + 8;
      if not NativeBoundedSid(Sid, AceAddress, Header.Size) then begin
        Result := NativeSecurityRefusal(Path, 42); Exit;
      end;
      if not NativeTrustedSid(Sid, StrictProduct) then begin Result := NativeSecurityRefusal(Path, 42); Exit; end;
    end;
    Result := True;
  finally
    if Descriptor <> 0 then NativeLocalFree(Descriptor);
  end;
end;

function NativeVerifyHandleType(Handle: THandle; Path: String; Directory: Boolean): Boolean;
var Info: TNativeFileInfo;
begin
  Result := False;
  NativeAcquireError := 'Unsafe or changed path type: "' + Path + '".';
  if NativeGetFileType(Handle) <> 1 then Exit;
  if NativeGetFileInformation(Handle, Info) = 0 then begin
    Result := NativeWindowsRefusal('Cannot inspect opened path "' + Path + '"', DLLGetLastError); Exit;
  end;
  if (Info.Attributes and $400) <> 0 then Exit;
  if Directory then Result := (Info.Attributes and $10) <> 0
  else Result := ((Info.Attributes and $10) = 0) and (Info.Links = 1) and
    (Info.SizeHigh = 0) and (Info.SizeLow = 0);
end;

function NativeVerifyReadFileType(Handle: THandle; Path: String; Maximum: LongWord): Boolean;
var Info: TNativeFileInfo;
begin
  Result := False;
  NativeAcquireError := 'Protected file is not a bounded regular single-link file: "' + Path + '".';
  if NativeGetFileType(Handle) <> 1 then Exit;
  if NativeGetFileInformation(Handle, Info) = 0 then begin
    Result := NativeWindowsRefusal('Cannot inspect protected file "' + Path + '"', DLLGetLastError); Exit;
  end;
  Result := ((Info.Attributes and $410) = 0) and (Info.Links = 1) and
    (Info.SizeHigh = 0) and (Info.SizeLow > 0) and (Info.SizeLow <= Maximum);
end;

function NativePinReadFile(Path: String; Maximum: LongWord; var Handle: THandle): Boolean;
begin
  Result := False;
  // Read-data access plus READ-only sharing blocks both replacement and an
  // already-open or new content writer during stamp/hash/copy verification.
  Handle := CreateFileW(Path, $20081, 1, 0, 3, $00200080, 0);
  if Handle = THandle(-1) then begin
    Handle := 0; Result := NativeWindowsRefusal('Cannot pin protected file "' + Path + '"', DLLGetLastError); Exit;
  end;
  Result := NativeVerifyReadFileType(Handle, Path, Maximum) and
    NativeVerifyHandleSecurity(Handle, Path, True, False) and
    NativeVerifyReadFileType(Handle, Path, Maximum);
  if not Result then begin CloseHandle(Handle); Handle := 0; end;
end;

procedure NativeKeepPin(Handle: THandle);
var I: Integer;
begin
  I := GetArrayLength(NativePathPins);
  SetArrayLength(NativePathPins, I+1); NativePathPins[I] := Handle;
end;

function NativeStrictProductPath(Path: String): Boolean;
begin
  Result := (Path = NativeState) or
    (Path = ExpandConstant('{localappdata}\LightImageTool')) or
    (Path = RemoveBackslashUnlessRoot(ExpandConstant('{app}')));
end;

function NativeCanonicalLocalPath(Path: String): Boolean;
var Remaining, Part, Stem: String; I, Separator, Components: Integer;
begin
  Result := False;
  if (Length(Path) < 3) or (Length(Path) > 32760) then Exit;
  if not (((Path[1] >= 'A') and (Path[1] <= 'Z')) or
          ((Path[1] >= 'a') and (Path[1] <= 'z'))) then Exit;
  if (Path[2] <> ':') or (Path[3] <> '\') then Exit;
  if Length(Path) = 3 then begin Result := True; Exit; end;
  if Path[Length(Path)] = '\' then Exit;
  Remaining := Copy(Path, 4, Length(Path)-3);
  Components := 2;
  while Remaining <> '' do begin
    Components := Components + 1;
    if Components > 128 then Exit;
    Separator := Pos('\', Remaining);
    if Separator = 0 then begin Part := Remaining; Remaining := ''; end
    else begin Part := Copy(Remaining, 1, Separator-1); Delete(Remaining, 1, Separator); end;
    if (Part = '') or (Part = '.') or (Part = '..') or (Length(Part) > 255) then Exit;
    if (Part[Length(Part)] = '.') or (Part[Length(Part)] = ' ') then Exit;
    for I := 1 to Length(Part) do
      if (Ord(Part[I]) < 32) or (Ord(Part[I]) = 127) or (Pos(Part[I], '<>:"/|?*') <> 0) then Exit;
    Separator := Pos('.', Part);
    Stem := Uppercase(Part);
    if Separator > 0 then Stem := Uppercase(Copy(Part, 1, Separator-1));
    if (Stem = 'CON') or (Stem = 'PRN') or (Stem = 'AUX') or (Stem = 'NUL') then Exit;
    if Length(Stem) = 4 then
      if ((Copy(Stem, 1, 3) = 'COM') or (Copy(Stem, 1, 3) = 'LPT')) and
         (Pos(Stem[4], '123456789' + #$00B9 + #$00B2 + #$00B3) <> 0) then Exit;
  end;
  Result := True;
end;

function PinNativeAncestors(Path: String): Boolean;
var Handle: THandle; Parent: String; J: Integer; Code: LongWord; Paths: TStringList;
begin
  Result := False;
  if not NativeCanonicalLocalPath(Path) then begin
    NativeAcquireError := 'Protected paths must use canonical absolute local-drive names without aliases: "' + Path + '".'; Exit;
  end;
  Paths := TStringList.Create;
  try
    while Path <> '' do begin
      if Paths.Count >= 256 then begin NativeAcquireError := 'Protected path has too many ancestors.'; Exit; end;
      Paths.Add(Path);
      Parent := ExtractFileDir(Path);
      if Parent = Path then Break;
      Path := Parent;
    end;
    // FILE_LIST_DIRECTORY is essential: metadata-only handles do not enforce
    // deny-delete sharing on Windows. Each opened handle, not a path precheck,
    // proves directory type, no reparse, and the appropriate owner/DACL policy.
    for J := Paths.Count-1 downto 0 do begin
      Path := Paths[J];
      // READ-only sharing rejects new or already-open directory data writers.
      // Attribute-only rights are still rejected by NativeVerifyHandleSecurity.
      Handle := CreateFileW(Path, $20081, 1, 0, 3, $02200000, 0);
      if Handle = THandle(-1) then begin
        Code := DLLGetLastError;
        if (Code = 2) or (Code = 3) then Continue;
        Result := NativeWindowsRefusal('Cannot pin directory "' + Path + '"', Code); Exit;
      end;
      if not NativeVerifyHandleType(Handle, Path, True) or
         not NativeVerifyHandleSecurity(Handle, Path, NativeStrictProductPath(Path), False) or
         not NativeVerifyHandleType(Handle, Path, True) then begin
        CloseHandle(Handle); Exit;
      end;
      NativeKeepPin(Handle);
    end;
    Result := True;
  finally
    Paths.Free;
  end;
end;

function PinExistingNativeLock(Path: String): Boolean;
var Handle: THandle; Code: LongWord;
begin
  Result := False;
  // FILE_READ_DATA makes this a lifetime share-enforcing leaf pin too.
  Handle := CreateFileW(Path, $20081, 3, 0, 3, $00200080, 0);
  if Handle = THandle(-1) then begin
    Code := DLLGetLastError;
    if (Code = 2) or (Code = 3) then begin Result := True; Exit; end;
    Result := NativeWindowsRefusal('Cannot inspect existing lock "' + Path + '"', Code); Exit;
  end;
  if not NativeVerifyHandleType(Handle, Path, False) or
     not NativeVerifyHandleSecurity(Handle, Path, True, False) or
     not NativeVerifyHandleType(Handle, Path, False) then begin CloseHandle(Handle); Exit; end;
  NativeKeepPin(Handle); Result := True;
end;

function EnsureNativeProductDirectory(Path: String; AllowCreate: Boolean): Boolean;
var Handle: THandle; Code: LongWord; Created: Boolean;
begin
  Result := False; Created := False;
  Handle := CreateFileW(Path, $20081, 1, 0, 3, $02200000, 0);
  if Handle = THandle(-1) then begin
    Code := DLLGetLastError;
    if ((Code <> 2) and (Code <> 3)) or not AllowCreate then begin
      Result := NativeWindowsRefusal('Cannot open required product directory "' + Path + '"', Code); Exit;
    end;
    // The fixed parent was already pinned and audited. Existing objects are
    // never reset: an ALREADY_EXISTS race is reopened and checked by handle.
    Created := NativeCreateDirectory(Path, NativeNewAttributes) <> 0;
    if not Created then begin
      Code := DLLGetLastError;
      if Code <> 183 then begin Result := NativeWindowsRefusal('Cannot create private directory "' + Path + '"', Code); Exit; end;
    end;
    Handle := CreateFileW(Path, $20081, 1, 0, 3, $02200000, 0);
    if Handle = THandle(-1) then begin Result := NativeWindowsRefusal('Cannot pin product directory "' + Path + '"', DLLGetLastError); Exit; end;
  end;
  if not NativeVerifyHandleType(Handle, Path, True) or
     not NativeVerifyHandleSecurity(Handle, Path, True, Created) or
     not NativeVerifyHandleType(Handle, Path, True) then begin CloseHandle(Handle); Exit; end;
  NativeKeepPin(Handle); Result := True;
end;

function TakeNativeLock(Path: String; var Handle: THandle; AllowCreate: Boolean): Boolean;
var Creation, Code: LongWord; Created: Boolean;
begin
  Result := False;
  // OPEN_ALWAYS only after the private parent and any existing leaf were
  // audited. OPEN_REPARSE_POINT applies to the object actually opened.
  Creation := 3;
  if AllowCreate then Creation := 4;
  Handle := NativeCreateSecureFile(Path, $C0000000, 3, NativeNewAttributes, Creation, $00200080, 0);
  Code := DLLGetLastError;
  Created := AllowCreate and (Code = 0);
  if Handle = THandle(-1) then begin
    Handle := 0; Result := NativeWindowsRefusal('Cannot open lock "' + Path + '"', Code); Exit;
  end;
  if NativeVerifyHandleType(Handle, Path, False) and NativeVerifyHandleSecurity(Handle, Path, True, Created) and
     NativeVerifyHandleType(Handle, Path, False) then begin
    // Covers the same full byte range as Rust File::try_lock/LockFileEx.
    Result := NativeLockFile(Handle, 0, 0, $FFFFFFFF, $FFFFFFFF) <> 0;
    if not Result then begin
      Code := DLLGetLastError;
      if Code = 33 then NativeAcquireError := 'Another application, worker or installer holds a required lock. Close it normally and retry.'
      else Result := NativeWindowsRefusal('Cannot lock "' + Path + '"', Code);
    end;
  end;
  if not Result then begin CloseHandle(Handle); Handle := 0; end;
end;

function AcquireNativeLocks(AllowCreate: Boolean): Boolean;
begin
  Result := NativeLocksHeld;
  if Result then Exit;
  NativeAcquireError := '';
  NativeState := ExpandConstant('{localappdata}\LightImageTool\updater-state');
  try
    try
      if not InitializeNativeSecurity then Exit;
      // Audit every existing directory and lock before the first creation.
      if not PinNativeAncestors(NativeState) or
         not PinNativeAncestors(RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) or
         not PinExistingNativeLock(NativeState + '\maintenance.lock') or
         not PinExistingNativeLock(NativeState + '\sessions.lock') then Exit;
      if not EnsureNativeProductDirectory(ExpandConstant('{localappdata}\LightImageTool'), AllowCreate) or
         not EnsureNativeProductDirectory(NativeState, AllowCreate) then Exit;
      if not TakeNativeLock(NativeState + '\maintenance.lock', NativeMaintenance, AllowCreate) or
         not TakeNativeLock(NativeState + '\sessions.lock', NativeSessions, AllowCreate) then Exit;
      NativeLocksHeld := True;
      NativeAcquireError := '';
      Result := True;
    except
      NativeAcquireError := GetExceptionMessage;
      Result := False;
    end;
  finally
    if not Result then begin
      Log(NativeAcquireError);
      ReleaseNativeLocks;
    end;
  end;
end;

function NativeCommand(Operation: String; var Code: Integer): Boolean;
var Parameters: String;
begin
  NativeLaunchError := '';
  Code := 0;
  Result := False;
  if not NativeLocksHeld then begin
    NativeLaunchError := 'Native transaction ' + Operation + ' was not launched because the required locks are not held.';
    Log(NativeLaunchError); Exit;
  end;
  Parameters := '--installer-transaction ' + Operation + ' --install-dir "' +
    RemoveBackslashUnlessRoot(ExpandConstant('{app}')) + '" --parent-pid ' +
    IntToStr(NativeGetCurrentProcessId);
  if (Operation = 'prepare') or (Operation = 'reconcile') then begin
    Parameters := Parameters + ' --plan "' + NativePlan + '" --package "' + ExpandConstant('{srcexe}') + '" --explorer ';
    if WizardIsTaskSelected('explorer') then Parameters := Parameters + '1'
    else Parameters := Parameters + '0';
  end;
  // Exec returns a Windows error on False, and a process exit code on True
  // with ewWaitUntilTerminated. Never interpret a launch error as helper output.
  // https://jrsoftware.org/ishelp/topic_isxfunc_exec.htm
  Result := Exec(NativeHelper, Parameters, '', SW_HIDE, ewWaitUntilTerminated, Code);
  if not Result then begin
    NativeLaunchError := 'Native transaction ' + Operation + ' could not launch (Windows error ' +
      IntToStr(Code) + ': ' + SysErrorMessage(Code) + ').';
    Log(NativeLaunchError);
  end
  else Log('Native transaction ' + Operation + ' returned ' + IntToStr(Code));
end;

function InstallStableLauncher: Boolean;
begin
  // First bounded v0.6 migration or clean install only. A normal update must
  // keep the only recovery entry point unchanged even if its package has newer bytes.
  Result := RecoveryReady and NativeBootstrap;
end;

function NativeReconcileExitError(Code: Integer): String;
begin
  Result := 'Native transaction reconcile failed with helper exit code ' + IntToStr(Code) + '.';
  // Only decode fixed diagnostics after successful Exec, never launch errors.
  Result := Result + NativeSecurityExplanation(Code);
end;

function RecoveryReconcile: String;
var Code: Integer;
begin
  Result := '';
  if RecoveryReady then Exit;
  if not AcquireNativeLocks(True) then begin
    Result := NativeAcquireError; Exit;
  end;
  try
    ExtractTemporaryFile('gloamere-image-recovery.exe');
    ExtractTemporaryFile('installer-plan.json');
    NativeHelper := ExpandConstant('{tmp}\gloamere-image-recovery.exe');
    NativePlan := ExpandConstant('{tmp}\installer-plan.json');
    if not NativeCommand('reconcile', Code) then
      Result := NativeLaunchError
    else if Code <> 0 then
      Result := NativeReconcileExitError(Code);
    // A persisted last-error file may be from an older command. Preserve it,
    // but do not present its contents as evidence of this reconcile attempt.
    if Result <> '' then begin
      Result := Result + ' Preserve the Setup log, plus any helper logs, journals and snapshots in ' + NativeState + '.';
      Log(Result);
    end;
  except
    Result := GetExceptionMessage;
  end;
end;

function RecoveryBegin: String;
var Code: Integer;
begin
  Result := '';
  if RecoveryReady then Exit;
  if DirExists(ExpandConstant('{app}\.light-image-upgrade-recovery')) then begin
    Result := 'An earlier installer recovery directory exists. Preserve it and repair that installation before continuing.'; Exit;
  end;
  if not AcquireNativeLocks(True) then begin
    Result := NativeAcquireError; Exit;
  end;
  try
    if NativeHelper = '' then RaiseException('Native helper was not prepared.');
    if not NativeCommand('prepare', Code) or ((Code <> 0) and (Code <> 10)) then begin
      Result := 'The owned installation could not be safely snapshotted. No installation started. Preserve logs and recovery data in ' + NativeState; Exit;
    end;
    NativeBootstrap := Code = 10;
    RecoveryReady := True;
  except
    Result := GetExceptionMessage;
  end;
end;

procedure RecoveryStart;
var Code: Integer;
begin
  if not RecoveryReady or not NativeCommand('begin', Code) or (Code <> 0) then
    RaiseException('Cannot durably begin installation. Preserve the native recovery journal.');
  if not PinNativeAncestors(ExpandConstant('{app}')) then
    RaiseException('Installation directory changed before file copy.');
  RecoveryStarted := True;
end;

procedure RecoveryInstalled;
var Code: Integer;
begin
  if not NativeCommand('pending-health', Code) or (Code <> 0) then
    RaiseException('Installed files or registration did not pass readback. Preserve the native recovery journal.');
  NativePendingHealth := True;
end;

procedure DeinitializeSetup;
var Code: Integer;
begin
  try
    if RecoveryReady and not RecoverySuccess then begin
      if not NativeCommand('recover', Code) or (Code <> 0) then begin
        Log('Recovery incomplete; preserve ' + NativeState);
        SuppressibleMsgBox('Installation recovery needs attention. No restart or forced close was requested. Preserve logs and all recovery data in ' + NativeState,
          mbError, MB_OK, IDOK);
      end;
    end;
    // Even successful Setup keeps every snapshot until actual UI health.
    if RecoverySuccess then Log('Installation staged. Waiting for next-launch UI health confirmation; snapshot retained.');
  finally
    ReleaseNativeLocks;
  end;
end;

function VerifiedInstalledLauncher: Boolean;
var Path, Hash: String; Lines: TArrayOfString; I: Integer;
begin
  Result := False;
  ReleaseNativeBootstrapPins;
  if not NativeLocksHeld then begin
    NativeAcquireError := 'Cannot verify the launcher before protected state admission.'; Exit;
  end;
  try
    Path := NativeState + '\launcher.sha256';
    if not NativePinReadFile(Path, 256, NativeStampPin) then Exit;
    NativeAcquireError := 'The protected launcher stamp is missing, malformed or belongs to another product.';
    if not LoadStringsFromFile(Path, Lines) or (GetArrayLength(Lines) <> 2) then Exit;
    if Lines[0] <> OwnerId then Exit;
    Hash := Lines[1];
    if Length(Hash) <> 64 then Exit;
    for I := 1 to Length(Hash) do
      if not (((Hash[I] >= '0') and (Hash[I] <= '9')) or ((Hash[I] >= 'a') and (Hash[I] <= 'f'))) then Exit;
    if not NativePinReadFile(InstalledExe, NativeLauncherMaxBytes, NativeLauncherPin) then Exit;
    NativeAcquireError := 'The stable launcher differs from its protected identity stamp.';
    Result := GetSHA256OfFile(InstalledExe) = Hash;
    if Result then begin NativeVerifiedLauncherHash := Hash; NativeAcquireError := ''; end;
  finally
    if not Result then ReleaseNativeBootstrapPins;
  end;
end;

function NativeUninstallPreflight: Boolean;
var Code: Integer;
begin
  Result := AcquireNativeLocks(False);
  if not Result then Exit;
  Result := False;
  try
    try
      if not VerifiedInstalledLauncher then Exit;
      NativeHelper := ExpandConstant('{tmp}\gloamere-image-uninstall-recovery.exe');
      if not CopyFile(InstalledExe, NativeHelper, True) then begin
        NativeAcquireError := 'Could not copy the verified stable launcher to the private uninstall temporary directory.'; Exit;
      end;
      if GetSHA256OfFile(NativeHelper) <> NativeVerifiedLauncherHash then begin
        NativeAcquireError := 'The temporary uninstall helper differs from the protected launcher identity.'; Exit;
      end;
      // Keep both source leaves immutable until the private copy is checked,
      // then release before native removal can remove the original launcher.
      ReleaseNativeBootstrapPins;
      if not NativeCommand('uninstall-check', Code) then NativeAcquireError := NativeLaunchError
      else if Code <> 0 then NativeAcquireError := 'Native transaction uninstall-check failed with helper exit code ' +
        IntToStr(Code) + '.' + NativeSecurityExplanation(Code)
      else begin Result := True; NativeAcquireError := ''; end;
    except
      NativeAcquireError := GetExceptionMessage;
      Result := False;
    end;
  finally
    ReleaseNativeBootstrapPins;
    if not Result then begin Log(NativeAcquireError); ReleaseNativeLocks; end;
  end;
end;

procedure DeinitializeUninstall;
begin
  ReleaseNativeLocks;
end;

procedure NativeRemoveLauncher;
var Code: Integer;
begin
  if not NativeCommand('uninstall-remove-launcher', Code) or (Code <> 0) then
    Log('The changed or unavailable stable launcher was preserved after uninstall.');
end;

function NativeCanOpenDefaults: Boolean;
begin
  Result := NativePendingHealth and not NativeFailureDetected;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if NativeFailureDetected then Result := 20;
end;
