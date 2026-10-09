param(
 [Parameter(Mandatory=$true)][string]$Executable,
 [Parameter(Mandatory=$true)][string]$Image,
 [Parameter(Mandatory=$true)][string]$OutputDirectory,
 [string]$WorkflowImage
)
$ErrorActionPreference='Stop'
$Executable=[IO.Path]::GetFullPath($Executable)
$Image=[IO.Path]::GetFullPath($Image)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $OutputDirectory){throw 'Use a fresh fault output directory'}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$sourceHash=(Get-FileHash -LiteralPath $Image).Hash
$records=[Collections.Generic.List[object]]::new()
$modes=@('gpu-init-failure','destroy-device')
if($WorkflowImage){$modes+='destroy-device-during-export'}
foreach($mode in $modes) {
 $directory=Join-Path $OutputDirectory $mode
 New-Item -ItemType Directory -Path $directory -Force | Out-Null
 $stderr=Join-Path $directory 'stderr.log'
 $arguments=@('--smoke',('"'+$directory+'"'),('"'+$Image+'"'),'1',('--'+$mode))
 if($mode -eq 'destroy-device-during-export'){$arguments+=@('--workflow-image',('"'+[IO.Path]::GetFullPath($WorkflowImage)+'"'))}
 $watch=[Diagnostics.Stopwatch]::StartNew()
 $faultProcess=Start-Process -FilePath $Executable -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardError $stderr -RedirectStandardOutput (Join-Path $directory 'stdout.log')
 $timedOut=$false
 while(-not $faultProcess.HasExited) {
  if($watch.Elapsed.TotalSeconds -gt 30){$timedOut=$true;Stop-Process -Id $faultProcess.Id -Force;break}
  Start-Sleep -Milliseconds 100
 }
 $faultProcess.WaitForExit()
 $diagnostic=Get-Content -LiteralPath $stderr -Raw -Encoding utf8
 $callbackPath=Join-Path $directory 'gpu-loss.json'
 $callback=if(Test-Path -LiteralPath $callbackPath){Get-Content -LiteralPath $callbackPath -Raw -Encoding utf8 | ConvertFrom-Json}else{$null}
 $boundaryPath=Join-Path $directory 'runtime-failure.json'
 $boundary=if(Test-Path -LiteralPath $boundaryPath){Get-Content -LiteralPath $boundaryPath -Raw -Encoding utf8 | ConvertFrom-Json}else{$null}
 $readyPath=Join-Path $directory 'gpu-loss-ready.json'
 $ready=if(Test-Path -LiteralPath $readyPath){Get-Content -LiteralPath $readyPath -Raw -Encoding utf8 | ConvertFrom-Json}else{$null}
 $unchanged=(Get-FileHash -LiteralPath $Image).Hash -eq $sourceHash
 if(-not $unchanged){throw 'Source identity changed during controlled fault'}
 $records.Add([pscustomobject]@{
  mode=$mode;exit_code=$faultProcess.ExitCode;elapsed_ms=$watch.ElapsedMilliseconds;
  timed_out=$timedOut;original_unchanged=$unchanged;
  controlled_logical_device_destroy=($null -ne $ready);
  export_in_flight_when_fault_scheduled=($null -ne $ready -and $ready.export_in_flight);
  export_format_when_fault_scheduled=$ready.export_format;
  jpeg_target_published=(Test-Path -LiteralPath (Join-Path $directory 'workflow-crop.jpg'));
  png_target_published=(Test-Path -LiteralPath (Join-Path $directory 'workflow-crop.png'));
  unpublished_temp_count=@(Get-ChildItem -LiteralPath $directory -Filter '.light-image-export-*.tmp' -Force).Count;
  device_callback_observed=($null -ne $callback);device_callback_reason=$callback.reason;
  panic_observed=($diagnostic -match 'panicked at');
  panic_caught_by_boundary=($null -ne $boundary -and $boundary.panic_caught_by_boundary);
  unhandled_panic=(($diagnostic -match 'panicked at') -and -not $boundary.panic_caught_by_boundary);
  initialization_error_reported=($mode -eq 'gpu-init-failure' -and $null -ne $boundary -and $boundary.failure_reported);
  real_driver_loss_tested=$false;native_input_tested=$false
 })
 Write-Output ('Completed controlled '+$mode)
}
$records | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'controlled-fault-results.json') -Encoding utf8
$restart=Join-Path $OutputDirectory 'normal-restart'
& (Join-Path $PSScriptRoot 'run-smoke.ps1') -Executable $Executable -Image $Image -OutputDirectory $restart -IdleSeconds 5 *> (Join-Path $OutputDirectory 'normal-restart.log')
[pscustomobject]@{normal_restart_rendered=$true;original_unchanged=((Get-FileHash -LiteralPath $Image).Hash -eq $sourceHash);real_driver_loss_tested=$false} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'restart-result.json') -Encoding utf8
$records | ConvertTo-Json -Depth 6
foreach($record in $records) {
 if($record.timed_out -or $record.exit_code -ne 1 -or $record.unhandled_panic){throw ('Controlled fault failed: '+$record.mode)}
 if($record.mode -eq 'gpu-init-failure') {
  if(-not $record.initialization_error_reported){throw 'Initialization fault was not reported'}
 } elseif(-not $record.device_callback_observed -or -not $record.panic_caught_by_boundary) {throw ('Device fault boundary was not observed: '+$record.mode)}
 if($record.mode -eq 'destroy-device-during-export' -and (-not $record.export_in_flight_when_fault_scheduled -or $record.export_format_when_fault_scheduled -ne 'Jpeg' -or $record.jpeg_target_published)){throw 'In-flight JPEG cancellation was not proved'}
}
exit 0
