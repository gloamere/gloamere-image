param(
 [Parameter(Mandatory=$true)][string]$BaselineExecutable,
 [Parameter(Mandatory=$true)][string]$OptimizedExecutable,
 [Parameter(Mandatory=$true)][string]$Image,
 [Parameter(Mandatory=$true)][string]$OutputDirectory,
 [int]$Repeats=3
)
$ErrorActionPreference='Stop'
$BaselineExecutable=[IO.Path]::GetFullPath($BaselineExecutable)
$OptimizedExecutable=[IO.Path]::GetFullPath($OptimizedExecutable)
$Image=[IO.Path]::GetFullPath($Image)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$identity=@{baseline_sha256=(Get-FileHash -LiteralPath $BaselineExecutable).Hash;optimized_sha256=(Get-FileHash -LiteralPath $OptimizedExecutable).Hash;image_sha256=(Get-FileHash -LiteralPath $Image).Hash;idle_seconds=20;repeats=$Repeats;native_input_tested=$false}
$identity | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'identity.json') -Encoding utf8
for($round=0;$round -lt $Repeats;$round++) {
 $variants=if($round % 2 -eq 0){@('baseline','optimized-performance','optimized-memory-usage')}else{@('optimized-memory-usage','optimized-performance','baseline')}
 foreach($variant in $variants) {
  $directory=Join-Path $OutputDirectory ($variant+'-'+$round)
  $parameters=@{Executable=$(if($variant -eq 'baseline'){$BaselineExecutable}else{$OptimizedExecutable});Image=$Image;OutputDirectory=$directory;ShowInfo=$true;IdleSeconds=20}
  if($variant -eq 'optimized-memory-usage'){$parameters.GpuMemoryUsage=$true}
  if($variant -eq 'optimized-performance'){$parameters.GpuPerformance=$true}
  & (Join-Path $PSScriptRoot 'run-smoke.ps1') @parameters *> (Join-Path $OutputDirectory ($variant+'-'+$round+'.log'))
  Write-Output ('Completed '+$variant+' round '+$round)
 }
}
