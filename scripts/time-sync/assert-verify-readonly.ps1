$p = Join-Path $PSScriptRoot "verify-windows-time.ps1"
$text = Get-Content -Raw $p
$banned = @('sync-windows-time.ps1', '/config', 'Restart-Service', '/resync')
foreach ($b in $banned) {
    if ($text -match [regex]::Escape($b)) {
        throw "verify 脚本包含禁止内容: $b"
    }
}
Write-Output "verify readonly ok"
