param(
    [Parameter(Mandatory=$true)][ValidateSet('stop','start','restart','query')][string]$Action,
    [string]$Log
)
if (-not $Log) { $Log = Join-Path $PSScriptRoot 'svc_ctl.log' }
$out = & sc.exe $Action paveconel 2>&1
$s = & sc.exe query paveconel 2>&1
$startType = (& sc.exe qc paveconel 2>&1 | Select-String 'START_TYPE') -join ''
$text = "=== sc $Action paveconel ===`n" + ($out -join "`n") + "`n" + ($s -join "`n") + "`n$startType`n"
$text | Add-Content -Path $Log -Encoding utf8
$text