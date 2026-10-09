// One bounded value list is shared by preflight, recovery, writes and uninstall.
type
  TDefaultAppValue = record
    Key, Name, Text: String;
    Removed: Boolean;
  end;
  TDefaultAppValues = array of TDefaultAppValue;
var
  DefaultAppWrites: TDefaultAppValues;
