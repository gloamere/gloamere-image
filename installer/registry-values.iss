// Read raw value types in the same HKCU view used by Inno's registry functions.
function ProductRegOpen(Key: LongWord; Subkey: String; Options, Access: LongWord;
  var Handle: LongWord): LongWord;
  external 'RegOpenKeyExW@advapi32.dll stdcall';
function ProductRegQuery(Handle: LongWord; Name: String; Reserved: Integer;
  var Kind: LongWord; Data: Integer; var Size: LongWord): LongWord;
  external 'RegQueryValueExW@advapi32.dll stdcall';
function ProductRegClose(Handle: LongWord): LongWord;
  external 'RegCloseKey@advapi32.dll stdcall';

function ProductRegistryReadAccess: LongWord;
begin
  Result := $0001; // KEY_QUERY_VALUE, no write or subkey-enumeration permission.
  if Is64BitInstallMode then Result := Result or $0100 // KEY_WOW64_64KEY
  else if IsWin64 then Result := Result or $0200; // KEY_WOW64_32KEY
end;

function QueryHkcuValueKind(Key, Name: String; var Kind: LongWord): Boolean;
var Handle, Size: LongWord;
begin
  Result := False; Kind := 0;
  if ProductRegOpen($80000001, Key, 0, ProductRegistryReadAccess, Handle) <> 0 then Exit;
  try
    Size := 0;
    if ProductRegQuery(Handle, Name, 0, Kind, 0, Size) <> 0 then Exit;
    Result := True;
  finally ProductRegClose(Handle); end;
end;
