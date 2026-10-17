param(
    [string]$Exe,
    [string]$Log
)

# Defaults point at this checkout; override either with -Exe / -Log.
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\paveconel.exe' }
if (-not $Log) { $Log = Join-Path $PSScriptRoot 'svc_install.log' }

function Log($m) { $m | Out-File -FilePath $Log -Append -Encoding utf8 }
Remove-Item $Log -ErrorAction SilentlyContinue

Log "=== elevate check ==="
Log "elevated: $(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"

Log "=== stop NBFC (avoid two EC owners) ==="
Log ((sc.exe stop NbfcService 2>&1) -join "`n")
Start-Sleep -Seconds 3
Log ((sc.exe config NbfcService start= demand 2>&1) -join "`n")

Log "=== install paveconel service ==="
Log ((& $Exe install-service 2>&1) -join "`n")

Log "=== query before start ==="
Log ((sc.exe query paveconel 2>&1) -join "`n")

Log "=== start ==="
Log ((sc.exe start paveconel 2>&1) -join "`n")
Start-Sleep -Seconds 3

Log "=== query after start ==="
Log ((sc.exe query paveconel 2>&1) -join "`n")
