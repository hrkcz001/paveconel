param(
    [string]$Exe,
    [string]$Log
)
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\paveconel.exe' }
if (-not $Log) { $Log = Join-Path $PSScriptRoot 'svc_repoint.log' }
function Log($m) { $m | Out-File -FilePath $Log -Append -Encoding utf8 }
Remove-Item $Log -ErrorAction SilentlyContinue
Log "=== repoint binPath to $Exe ==="
Log ((sc.exe config paveconel binPath= "`"$Exe`" service" 2>&1) -join "`n")
Log "=== start ==="
Log ((sc.exe start paveconel 2>&1) -join "`n")
Start-Sleep -Seconds 3
Log ((sc.exe query paveconel 2>&1) -join "`n")
Log ((sc.exe qc paveconel 2>&1 | Select-String 'BINARY_PATH|START_TYPE') -join "`n")
