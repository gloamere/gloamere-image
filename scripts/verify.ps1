param([string]$BuildTarget)
$ErrorActionPreference='Stop'
if ($BuildTarget) {$env:LIGHT_IMAGE_BUILD_TARGET=$BuildTarget}
$projectRoot=Split-Path -Parent $PSScriptRoot
$workspaceRoot=Split-Path -Parent $projectRoot
& (Join-Path $PSScriptRoot "init-build-environment.ps1") -WorkspaceRoot $workspaceRoot
$scopedCargo=Join-Path $workspaceRoot 'toolchain\cargo'
$scopedRustup=Join-Path $workspaceRoot 'toolchain\rustup'
if ((Test-Path -LiteralPath (Join-Path $scopedCargo 'bin\cargo.exe')) -and
    (Test-Path -LiteralPath $scopedRustup)) {
    $env:CARGO_HOME=$scopedCargo
    $env:RUSTUP_HOME=$scopedRustup
    $env:PATH=(Join-Path $scopedCargo 'bin')+';'+$env:PATH
}
$env:CARGO_TERM_COLOR='never'
$env:CARGO_TARGET_DIR=if($env:LIGHT_IMAGE_BUILD_TARGET){$env:LIGHT_IMAGE_BUILD_TARGET}else{Join-Path $projectRoot 'target'}
Push-Location $projectRoot
try {
foreach($arguments in @(
    @('fmt','--all','--check'),
    @('clippy','--workspace','--all-targets','--','-D','warnings'),
    @('build','--workspace'),
    @('build','--workspace','--examples'),
    @('test','--workspace','--','--include-ignored','--test-threads=1')
)) {
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) {throw "Quality gate failed: $($arguments -join ' ')"}
}
} finally {Pop-Location}
