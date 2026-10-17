$ErrorActionPreference = 'Continue'
# Run elevated: for a session-0 service process `TotalProcessorTime` is unreadable from a
# normal shell and comes back empty rather than zero, so an unelevated probe reports 0 ms.
$log = Join-Path $PSScriptRoot 'load_probe.log'
Remove-Item $log -ErrorAction SilentlyContinue

function Write-ProbeLog {
    param([string]$m)
    $m | Out-File -FilePath $log -Append -Encoding utf8
}

function Get-ProcLoad {
    param([int]$ProcId, [int]$Secs, [string]$Label)
    $p1 = Get-Process -Id $ProcId -ErrorAction Stop
    $c1 = $p1.TotalProcessorTime.TotalMilliseconds
    $k1 = $p1.PrivilegedProcessorTime.TotalMilliseconds
    $h1 = $p1.HandleCount
    Start-Sleep -Seconds $Secs
    $p2 = Get-Process -Id $ProcId -ErrorAction Stop
    $d = $p2.TotalProcessorTime.TotalMilliseconds - $c1
    $dk = $p2.PrivilegedProcessorTime.TotalMilliseconds - $k1
    $pctOne = $d / ($Secs * 1000) * 100
    $pctAll = $pctOne / $cores
    Write-ProbeLog ("$Label : $([math]::Round($d,1)) ms CPU in $Secs s (privileged $([math]::Round($dk,1)) ms) = $([math]::Round($pctOne,4))% of one core = $([math]::Round($pctAll,5))% of all $cores cores")
    Write-ProbeLog ("$Label   memory: ws $([math]::Round($p2.WorkingSet64/1MB,1)) MB  private $([math]::Round($p2.PrivateMemorySize64/1MB,1)) MB  threads $($p2.Threads.Count)  handles $($p2.HandleCount)  handle_delta $($h1 - $p2.HandleCount)")
}

try {
    $cores = [Environment]::ProcessorCount
    $svcPid = (Get-CimInstance Win32_Service -Filter "Name='paveconel'").ProcessId
    $trayPid = (Get-Process paveconel -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $svcPid } | Select-Object -First 1).Id
    Write-ProbeLog "cores=$cores  service_pid=$svcPid  tray_pid=$trayPid"
    Get-ProcLoad -ProcId $svcPid -Secs 30 -Label "service"
    if ($trayPid) { Get-ProcLoad -ProcId $trayPid -Secs 30 -Label "tray" } else { Write-ProbeLog "tray: not running" }
} catch {
    Write-ProbeLog "ERROR: $($_.Exception.Message)"
}
