// One bounded value list is shared by preflight, recovery, writes and uninstall.
type
  // Preserve exact predecessor profiles; a partial mix never grants ownership.
  TDefaultAppProfile = (dapLegacy, dapV04, dapV05);
  TDefaultAppValue = record
    Key, Name, Text: String;
    Removed: Boolean;
  end;
  TDefaultAppValues = array of TDefaultAppValue;
var
  DefaultAppWrites: TDefaultAppValues;
