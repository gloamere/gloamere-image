param([Parameter(Mandatory=$true)][string]$WorkspaceRoot)
$ErrorActionPreference = "Stop"
if (($IsWindows -or $env:OS -eq "Windows_NT") -and -not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    $scopedVs = Join-Path $WorkspaceRoot 'toolchain\msvc'
    $vsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vsRoot = if (Test-Path -LiteralPath (Join-Path $scopedVs 'Common7\Tools\Launch-VsDevShell.ps1')) {
        $scopedVs
    } elseif (Test-Path -LiteralPath $vsWhere) {
        & $vsWhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    } else { $null }
    if (-not $vsRoot) { throw 'MSVC C++ Build Tools with Windows SDK is required' }
    & (Join-Path $vsRoot 'Common7\Tools\Launch-VsDevShell.ps1') -VsInstallationPath $vsRoot -SkipAutomaticLocation -Arch amd64 -HostArch amd64 | Out-Null
}
