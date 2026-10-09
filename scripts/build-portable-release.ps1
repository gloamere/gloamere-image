<# Deprecated developer compatibility wrapper. Runs the EXE-only release pipeline. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Compiler,
    [string]$BuildTarget,
    [string]$Output,
    [string]$PythonCommand = 'python',
    [string]$SourceCommit,
    [switch]$RunTests
)
$ErrorActionPreference = 'Stop'
Write-Warning 'build-portable-release.ps1 is deprecated. Use build-release.ps1. Tests always run; no portable ZIP is created.'
$arguments = @{ Compiler=$Compiler; PythonCommand=$PythonCommand }
if ($BuildTarget) { $arguments.BuildTarget=$BuildTarget }
if ($Output) { $arguments.Output=$Output }
if ($SourceCommit) { $arguments.SourceCommit=$SourceCommit }
& (Join-Path $PSScriptRoot 'build-release.ps1') @arguments
