param(
 [Parameter(Mandatory=$true)][string]$Executable,
 [Parameter(Mandatory=$true)][string]$Image,
 [Parameter(Mandatory=$true)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
$Executable=[IO.Path]::GetFullPath($Executable)
$Image=[IO.Path]::GetFullPath($Image)
$qaDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $qaDirectory){throw 'Use a fresh owned QA directory'}
New-Item -ItemType Directory -Path $qaDirectory | Out-Null
$destination=Join-Path $qaDirectory 'interrupted-copy.png'
$before=(Get-FileHash -LiteralPath $Image -Algorithm SHA256).Hash
$arguments=@('export',('"'+$Image+'"'),('"'+$destination+'"'),'png')
$watch=[Diagnostics.Stopwatch]::StartNew()
$qaProcess=Start-Process -FilePath $Executable -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $qaDirectory 'stdout.log') -RedirectStandardError (Join-Path $qaDirectory 'stderr.log')
$observed=$null
while(-not $qaProcess.HasExited -and $watch.Elapsed.TotalSeconds -lt 30){
    $observed=Get-ChildItem -LiteralPath $qaDirectory -Filter '.light-image-export-*.tmp' | Where-Object {$_.Length -gt 0} | Select-Object -First 1
    if($observed){break}
    Start-Sleep -Milliseconds 2
}
if(-not $observed){
    if(-not $qaProcess.HasExited){Stop-Process -Id $qaProcess.Id -Force}
    throw 'No partial temporary output observed; crash injection not verified'
}
Stop-Process -Id $qaProcess.Id -Force
$qaProcess.WaitForExit()
$after=(Get-FileHash -LiteralPath $Image -Algorithm SHA256).Hash
$remaining=@(Get-ChildItem -LiteralPath $qaDirectory -Filter '.light-image-export-*.tmp' | Select-Object Name,Length)
$report=[pscustomobject]@{test='actual process termination during partial encode';pid=$qaProcess.Id;observed_temp_bytes=$observed.Length;elapsed_ms=$watch.ElapsedMilliseconds;source_sha256_before=$before;source_sha256_after=$after;source_unchanged=($before -eq $after);final_destination_exists=(Test-Path -LiteralPath $destination);stale_temp_files=$remaining;stale_temp_expected_after_hard_kill=$true}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $qaDirectory 'result.json') -Encoding utf8
if($before -ne $after -or (Test-Path -LiteralPath $destination)){throw 'Crash safety failed'}
$retry=Join-Path $qaDirectory 'retry-copy.png'
& $Executable export $Image $retry png | Set-Content -LiteralPath (Join-Path $qaDirectory 'retry.log') -Encoding utf8
if($LASTEXITCODE -ne 0 -or -not(Test-Path -LiteralPath $retry)){throw 'Post-crash export recovery failed'}
& $Executable inspect $retry | Set-Content -LiteralPath (Join-Path $qaDirectory 'retry-inspect.json') -Encoding utf8
if($LASTEXITCODE -ne 0){throw 'Post-crash export readback failed'}
$report | ConvertTo-Json -Depth 5
