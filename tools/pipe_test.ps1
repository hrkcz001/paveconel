$ErrorActionPreference = 'Continue'
"=== pipes matching paveconel ==="
[System.IO.Directory]::GetFiles("\\.\pipe\") | Where-Object { $_ -match 'paveconel' }

"=== connect test ==="
$c = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'paveconel', [System.IO.Pipes.PipeDirection]::InOut)
try {
    $c.Connect(3000)
    "connected: $($c.IsConnected)"
    if ($c.IsConnected) {
        $sw = New-Object System.IO.StreamWriter($c)
        $sw.AutoFlush = $true
        $sr = New-Object System.IO.StreamReader($c)
        foreach ($cmd in @('STATUS','MODE curve','STATUS')) {
            $sw.WriteLine($cmd)
            $reply = $sr.ReadLine()
            "cmd '$cmd' -> '$reply'"
        }
        $sw.WriteLine('BYE')
    }
} catch {
    "connect failed: $($_.Exception.Message)"
} finally {
    $c.Dispose()
}
