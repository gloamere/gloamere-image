param(
 [Parameter(Mandatory=$true)][string]$Executable,
 [Parameter(Mandatory=$true)][string]$Image,
 [Parameter(Mandatory=$true)][string]$OutputDirectory,
 [double]$Scale=1,
 [switch]$ShowInfo,
 [switch]$ShowList,
 [switch]$ShowMore,
 [switch]$ShowEdit,
 [switch]$ShowExport,
 [switch]$ShowError,
 [switch]$JpegPreview,
 [switch]$SmallWindow,
 [switch]$GpuMemoryUsage,
 [switch]$GpuPerformance,
 [switch]$CropPreview,
 [switch]$CjkFallback,
 [string]$WorkflowImage,
 [ValidateRange(4,1000)][int]$WorkflowOpens=4,
 [ValidateRange(0,2000)][int]$WorkflowDelayMs=0,
 [ValidateRange(1,3600)][int]$TimeoutSeconds=60,
 [int]$IdleSeconds=20
)
$ErrorActionPreference='Stop'
$Executable=[IO.Path]::GetFullPath($Executable)
$Image=[IO.Path]::GetFullPath($Image)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
if(Test-Path -LiteralPath (Join-Path $OutputDirectory 'runtime.json')) {throw 'Use a fresh smoke output directory'}
$arguments=@('--smoke',('"'+$OutputDirectory+'"'),('"'+$Image+'"'),$Scale.ToString([Globalization.CultureInfo]::InvariantCulture),'--idle-seconds',$IdleSeconds.ToString())
if($ShowInfo){$arguments+='--show-info'}
if($ShowList){$arguments+='--show-list'}
if($ShowMore){$arguments+='--show-more'}
if($ShowEdit){$arguments+='--show-edit'}
if($ShowExport){$arguments+='--show-export'}
if($ShowError){$arguments+='--show-error'}
if($JpegPreview){$arguments+='--jpeg-preview'}
if($SmallWindow){$arguments+='--small-window'}
if($CjkFallback){$arguments+='--cjk-fallback'}
if($GpuMemoryUsage -and $GpuPerformance){throw 'Choose one GPU memory hint'}
if($GpuMemoryUsage){$arguments+='--gpu-memory-usage'}
if($GpuPerformance){$arguments+='--gpu-performance'}
if($CropPreview){$arguments+='--crop-preview'}
if($WorkflowImage){$arguments+=@('--workflow-opens',$WorkflowOpens.ToString(),'--workflow-delay-ms',$WorkflowDelayMs.ToString(),'--workflow-image',('"'+[IO.Path]::GetFullPath($WorkflowImage)+'"'))}
$watch=[Diagnostics.Stopwatch]::StartNew()
$qaProcess=Start-Process -FilePath $Executable -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $OutputDirectory 'stderr.log') -RedirectStandardOutput (Join-Path $OutputDirectory 'stdout.log')
$samples=[Collections.Generic.List[object]]::new()
try {
while(-not $qaProcess.HasExited) {
    $qaProcess.Refresh()
    $samples.Add([pscustomobject]@{elapsed_ms=$watch.ElapsedMilliseconds;cpu_ms=$qaProcess.TotalProcessorTime.TotalMilliseconds;working_set_bytes=$qaProcess.WorkingSet64;private_bytes=$qaProcess.PrivateMemorySize64;peak_working_set_bytes=$qaProcess.PeakWorkingSet64;peak_commit_bytes=$qaProcess.PeakPagedMemorySize64})
    if($watch.Elapsed.TotalSeconds -gt $TimeoutSeconds) {Stop-Process -Id $qaProcess.Id -Force;throw "Own smoke process exceeded configured timeout: $($qaProcess.Id)"}
    Start-Sleep -Milliseconds 100
}
$qaProcess.WaitForExit()
$runtime=Join-Path $OutputDirectory 'runtime.json'
if($qaProcess.ExitCode -ne 0 -or -not(Test-Path -LiteralPath $runtime)) {throw "Smoke failed, exit $($qaProcess.ExitCode)"}
$record=Get-Content -LiteralPath $runtime -Raw -Encoding utf8 | ConvertFrom-Json
if($record.error -or ($WorkflowImage -and -not $record.workflow.success)){throw "Own-app workflow/rendering failed; inspect runtime.json"}
$record | ConvertTo-Json -Depth 12
} catch {
    [pscustomobject]@{failed=$true;elapsed_ms=$watch.ElapsedMilliseconds;error=$_.Exception.Message;pid=$qaProcess.Id} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'failure.json') -Encoding utf8
    throw
} finally {
    $samples | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'process-samples.json') -Encoding utf8
}
