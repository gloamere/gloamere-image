<#
Build the only end-user install/update artifact: gloamere-image Setup EXE.
Prerequisites are supplied locally: Python 3.11+, locked Rust/MSVC + Windows SDK,
and official Inno Setup 6.7.3. Nothing is downloaded or installed by this script.
Example: powershell -NoProfile -File scripts/build-release.ps1 -Compiler 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
Output has internal evidence/staging and a release folder containing EXE, SHA256,
and reports. No portable installation archive is generated. Lifecycle/visual QA
must still be run on Windows and is never claimed by a successful build.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Compiler,
    [string]$BuildTarget,
    [string]$Output,
    [string]$PythonCommand = 'python',
    [string]$SourceCommit
)
$ErrorActionPreference = 'Stop'
if ($env:GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY) { throw 'GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY is forbidden for release builds; remove it and rerun.' }
if ($env:OS -ne 'Windows_NT') { throw 'Windows/MSVC is required for the release pipeline.' }
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$workspaceRoot = Split-Path -Parent $projectRoot
if (-not $Output) { $Output = Join-Path $projectRoot ('dist\windows-release-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
$outputRoot = [IO.Path]::GetFullPath($Output)
$distRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot 'dist')) + [IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.StartsWith($distRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Output must be a fresh directory beneath this project dist.' }
if (Test-Path -LiteralPath $outputRoot) { throw 'Use a fresh output directory; previous evidence is never replaced.' }
$ancestor = [IO.DirectoryInfo]$outputRoot
while ($null -ne $ancestor) {
    if ((Test-Path -LiteralPath $ancestor.FullName) -and ((Get-Item -LiteralPath $ancestor.FullName -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Output ancestors must not be symlinks, junctions or other reparse points.'
    }
    $ancestor = $ancestor.Parent
}
$compilerPath = (Resolve-Path -LiteralPath $Compiler).Path
if ([IO.Path]::GetFileName($compilerPath) -ine 'ISCC.exe') { throw 'Supply the installed official Inno Setup ISCC.exe.' }
$python = (Get-Command $PythonCommand -CommandType Application -ErrorAction Stop).Source
$previous = @{}
foreach ($key in @('CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS', 'CARGO_HOME', 'RUSTUP_HOME', 'CARGO_TARGET_DIR', 'CARGO_TERM_COLOR', 'PATH')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
$stages = [ordered]@{}
foreach ($name in @('python_preflight', 'environment_preflight', 'packaging_contracts', 'installer_contracts', 'migration_contracts', 'resource_contracts', 'format', 'clippy', 'debug_build', 'debug_tests', 'release_build', 'release_tests', 'pe_resources', 'dependency_tree', 'license_notices', 'stage_payload', 'validate_payload', 'compile_setup')) {
    $stages[$name] = 'not_run'
}
$activeStage = 'preflight'
$failure = $null
$internal = Join-Path $outputRoot 'internal'
$release = Join-Path $outputRoot 'release'
$logs = Join-Path $internal 'logs'
New-Item -ItemType Directory -Path $logs -Force | Out-Null
$report = [ordered]@{
    product = 'gloamere-image'; version = '0.4.0'; distribution = 'Setup EXE only'
    started_utc = [DateTime]::UtcNow.ToString('o'); status = 'running'
    signature_status = 'NotSigned'; stages = $stages
    windows_lifecycle_qa = 'not_run'; windows_visual_qa = 'not_run'
    limitations = @('Compilation is not installation/upgrade/rollback/uninstall acceptance.', 'Unsigned test build; no signing or trust validation claimed.')
}
function Invoke-Gate([string]$Name, [string]$Executable, [string[]]$Arguments) {
    $script:activeStage = $Name
    $script:stages[$Name] = 'running'
    # Windows PowerShell can represent native stderr as ErrorRecord even on
    # success. The native exit code is authoritative for these local tools.
    $local:ErrorActionPreference = 'Continue'
    $captured = @()
    & $Executable @Arguments 2>&1 | Tee-Object -Variable captured | Out-Host
    $code = $LASTEXITCODE
    $lines = [string[]]@()
    if ($null -ne $captured) { $lines = [string[]]@($captured) }
    [IO.File]::WriteAllLines((Join-Path $logs ($Name + '.log')), $lines, (New-Object Text.UTF8Encoding($false)))
    if ($code -ne 0) { throw "$Name failed with exit code $code; see internal logs." }
    $script:stages[$Name] = 'passed'
}
Push-Location $projectRoot
try {
    Invoke-Gate 'python_preflight' $python @('-c', 'import sys, tomllib; assert sys.version_info >= (3, 11), "Python 3.11+ required"')
    $activeStage = 'environment_preflight'
    & (Join-Path $PSScriptRoot 'init-build-environment.ps1') -WorkspaceRoot $workspaceRoot
    $scopedCargo = Join-Path $workspaceRoot 'toolchain\cargo'
    $scopedRustup = Join-Path $workspaceRoot 'toolchain\rustup'
    if ((Test-Path -LiteralPath (Join-Path $scopedCargo 'bin\cargo.exe')) -and (Test-Path -LiteralPath $scopedRustup)) {
        $env:CARGO_HOME = $scopedCargo
        $env:RUSTUP_HOME = $scopedRustup
        $env:PATH = (Join-Path $scopedCargo 'bin') + ';' + $env:PATH
    }
    $cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
    $cargoRoot = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $rustupRoot = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
    $targetRoot = if ($BuildTarget) { [IO.Path]::GetFullPath($BuildTarget) } else { Join-Path $projectRoot 'target' }
    $env:CARGO_TARGET_DIR = $targetRoot
    $env:CARGO_TERM_COLOR = 'never'
    $env:RUSTFLAGS = $null
    $flags = [Collections.Generic.List[string]]::new()
    $flags.Add('-C'); $flags.Add('target-feature=+crt-static')
    $mappings = @(
        @($workspaceRoot, '/build-workspace'), @([IO.Path]::GetFullPath($rustupRoot), '/rust-toolchain'),
        @([IO.Path]::GetFullPath($cargoRoot), '/cargo-home'), @($targetRoot, '/build-output'),
        @($projectRoot, '/gloamere-image'), @([IO.Path]::GetFullPath((Join-Path $cargoRoot 'registry\src')), '/third-party')
    )
    foreach ($mapping in $mappings) {
        $native = $mapping[0]; $portable = $native.Replace('\', '/')
        $flags.Add('--remap-path-prefix=' + $native + '=' + $mapping[1])
        if ($portable -ne $native) { $flags.Add('--remap-path-prefix=' + $portable + '=' + $mapping[1]) }
    }
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
    $target = 'x86_64-pc-windows-msvc'
    $report['architecture'] = $target
    $report['static_crt_requested'] = $true
    $provenance = [ordered]@{ source_commit = 'unversioned-working-tree'; git_dirty = $null }
    $git = Get-Command git -CommandType Application -ErrorAction SilentlyContinue
    if ($git -and (Test-Path -LiteralPath (Join-Path $projectRoot '.git'))) {
        $inside = & $git.Source rev-parse --is-inside-work-tree 2>$null
        if ($LASTEXITCODE -eq 0 -and $inside -eq 'true') {
            $provenance['source_commit'] = (& $git.Source rev-parse HEAD).Trim()
            if ($LASTEXITCODE -ne 0) { throw 'Could not read Git source revision.' }
            $status = & $git.Source status --porcelain
            if ($LASTEXITCODE -ne 0) { throw 'Could not inspect Git working-tree status.' }
            $provenance['git_dirty'] = [bool]$status
        }
    }
    if ($SourceCommit) {
        $provenance['source_commit'] = $SourceCommit
        $provenance['commit_supplied_by_builder'] = $true
    }
    if (Test-Path -LiteralPath (Join-Path $projectRoot 'SOURCE-MANIFEST.json')) {
        $archiveManifest = Get-Content -LiteralPath (Join-Path $projectRoot 'SOURCE-MANIFEST.json') -Raw | ConvertFrom-Json
        $provenance['source_archive_commit'] = $archiveManifest.source_commit
    }
    $report['provenance'] = $provenance
    $utf8 = New-Object Text.UTF8Encoding($false)
    $provenanceFile = Join-Path $internal 'provenance.json'
    [IO.File]::WriteAllText($provenanceFile, ($provenance | ConvertTo-Json -Depth 8), $utf8)
    $stages['environment_preflight'] = 'passed'
    Invoke-Gate 'packaging_contracts' $python @((Join-Path $PSScriptRoot 'test-release-packaging.py'))
    Invoke-Gate 'installer_contracts' $python @((Join-Path $PSScriptRoot 'test-default-apps-installer.py'))
    Invoke-Gate 'migration_contracts' $python @((Join-Path $PSScriptRoot 'test-branding-migration.py'))
    Invoke-Gate 'resource_contracts' $python @((Join-Path $projectRoot 'build-support\test_windows_resources.py'))
    Invoke-Gate 'format' $cargo @('fmt', '--all', '--check')
    Invoke-Gate 'clippy' $cargo @('clippy', '--workspace', '--all-targets', '--locked', '--offline', '--target', $target, '--', '-D', 'warnings')
    Invoke-Gate 'debug_build' $cargo @('build', '--workspace', '--all-targets', '--locked', '--offline', '--target', $target)
    Invoke-Gate 'debug_tests' $cargo @('test', '--workspace', '--all-targets', '--locked', '--offline', '--target', $target, '--', '--include-ignored', '--test-threads=1')
    Invoke-Gate 'release_build' $cargo @('build', '--workspace', '--release', '--all-targets', '--locked', '--offline', '--target', $target)
    Invoke-Gate 'release_tests' $cargo @('test', '--workspace', '--release', '--all-targets', '--locked', '--offline', '--target', $target, '--', '--include-ignored', '--test-threads=1')
    $binaryRoot = Join-Path $targetRoot ($target + '\release')
    Invoke-Gate 'pe_resources' $python @((Join-Path $projectRoot 'build-support\verify_windows_resources.py'), '--version', '0.4.0', (Join-Path $binaryRoot 'gloamere-image.exe'), (Join-Path $binaryRoot 'gloamere-image-worker.exe'))
    Invoke-Gate 'dependency_tree' $cargo @('tree', '--workspace', '--locked', '--offline', '--target', $target, '--edges', 'normal,build', '--prefix', 'none', '--format', '{p}')
    $licenseRoot = Join-Path $internal 'licenses'
    Invoke-Gate 'license_notices' $python @((Join-Path $PSScriptRoot 'collect-licenses.py'), '--tree', (Join-Path $logs 'dependency_tree.log'), '--registry', (Join-Path $cargoRoot 'registry\src'), '--cache', (Join-Path $projectRoot 'third-party-notices'), '--output', $licenseRoot)
    $validation = Join-Path $internal 'VALIDATION.md'
    $validationText = "# gloamere-image 0.4.0 build validation`n`n"
    foreach ($entry in $stages.GetEnumerator()) { $validationText += '- ' + $entry.Key + ': ' + $entry.Value + "`n" }
    $validationText += "`nStatic CRT requested for x86_64-pc-windows-msvc. Unsigned test build.`nWindows installation, upgrade, rollback, uninstall and visual QA: NOT RUN by this command.`nInno compile outcome and artifact hash appear in the accompanying installer-build.json.`n"
    [IO.File]::WriteAllText($validation, $validationText, $utf8)
    $payload = Join-Path $internal 'payload'
    Invoke-Gate 'stage_payload' $python @((Join-Path $PSScriptRoot 'stage-installer-payload.py'), '--release', $binaryRoot, '--licenses', $licenseRoot, '--output', $payload, '--commit', $provenance.source_commit, '--report', $validation, '--provenance', $provenanceFile)
    Invoke-Gate 'validate_payload' $python @((Join-Path $PSScriptRoot 'build-installer.py'), '--payload', $payload, '--validate-only')
    Invoke-Gate 'compile_setup' $python @((Join-Path $PSScriptRoot 'build-installer.py'), '--compiler', $compilerPath, '--payload', $payload, '--output', $release)
    $report['status'] = 'passed'
    $report['installer'] = Get-Content -LiteralPath (Join-Path $release 'installer-build.json') -Raw | ConvertFrom-Json
} catch {
    $failure = $_
    $stages[$activeStage] = 'failed'
    $report['status'] = 'failed'
    $report['failure'] = $_.Exception.Message
} finally {
    $report['finished_utc'] = [DateTime]::UtcNow.ToString('o')
    $serialized = $report | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText((Join-Path $outputRoot 'release-build.json'), $serialized, (New-Object Text.UTF8Encoding($false)))
    if ($report.status -eq 'passed') {
        [IO.File]::WriteAllText((Join-Path $release 'release-build.json'), $serialized, (New-Object Text.UTF8Encoding($false)))
    }
    foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process') }
    Pop-Location
}
if ($failure) { throw $failure }
Write-Host ('Setup EXE, SHA256 and build reports: ' + $release)
Write-Host 'Windows lifecycle and visual acceptance are still required before release.'
