param([Parameter(ValueFromRemainingArguments=$true)][string[]]$CargoArgs = @('test'))
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$workspaceRoot = Split-Path -Parent $taskRoot
& (Join-Path $PSScriptRoot "init-build-environment.ps1") -WorkspaceRoot $workspaceRoot
$scopedCargo = Join-Path $workspaceRoot 'toolchain\cargo'
$scopedRustup = Join-Path $workspaceRoot 'toolchain\rustup'
if ((Test-Path -LiteralPath (Join-Path $scopedCargo 'bin\cargo.exe')) -and
    (Test-Path -LiteralPath $scopedRustup)) {
    $env:CARGO_HOME = $scopedCargo
    $env:RUSTUP_HOME = $scopedRustup
    $env:PATH = (Join-Path $scopedCargo 'bin') + ';' + $env:PATH
}
$env:CARGO_TERM_COLOR = 'never'
$env:CARGO_TARGET_DIR = if ($env:LIGHT_IMAGE_BUILD_TARGET) { $env:LIGHT_IMAGE_BUILD_TARGET } else { Join-Path $taskRoot 'target' }
Push-Location $taskRoot
try { & cargo @CargoArgs; exit $LASTEXITCODE } finally { Pop-Location }
