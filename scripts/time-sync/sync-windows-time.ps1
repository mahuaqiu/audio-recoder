[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$NtpServer,
    [int]$Samples = 20,
    [double]$MaxOffsetMs = 5.0,
    [string]$OutputPath = ".\time-sync-report.json"
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail "请使用管理员 PowerShell 执行此脚本"
}
if ($Samples -lt 5 -or $MaxOffsetMs -le 0) {
    Fail "Samples 至少为 5，MaxOffsetMs 必须大于 0"
}

& w32tm /config "/manualpeerlist:$NtpServer,0x8" /syncfromflags:manual /update | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "配置 Windows Time 服务失败" }
Restart-Service W32Time -Force
& w32tm /resync /rediscover | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "Windows Time 同步失败" }

$raw = & w32tm /stripchart "/computer:$NtpServer" "/samples:$Samples" /dataonly 2>&1
if ($LASTEXITCODE -ne 0) { Fail "无法从 NTP server 获取 stripchart 样本`n$($raw -join "`n")" }

$values = @(
    $raw | ForEach-Object {
        $match = [regex]::Match([string]$_, '([+-]?\d+(?:\.\d+)?)s')
        if ($match.Success) { [double]$match.Groups[1].Value * 1000.0 }
    } | Where-Object { $_ -is [double] -and [double]::IsFinite($_) }
)
if ($values.Count -lt 5) { Fail "有效时间偏差样本不足，实际为 $($values.Count) 个" }

$sorted = @($values | Sort-Object)
$median = if ($sorted.Count % 2 -eq 1) {
    $sorted[[int]($sorted.Count / 2)]
} else {
    ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2.0
}
$maxAbs = ($values | ForEach-Object { [math]::Abs($_) } | Measure-Object -Maximum).Maximum
$status = if ($maxAbs -le $MaxOffsetMs) { "pass" } else { "fail" }
$nowNs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000000
$report = [ordered]@{
    schema_version = 1
    status = $status
    computer_name = $env:COMPUTERNAME
    ntp_server = $NtpServer
    checked_at_unix_ns = $nowNs
    sample_count = $Samples
    valid_sample_count = $values.Count
    median_offset_ms = [math]::Round($median, 6)
    max_abs_offset_ms = [math]::Round($maxAbs, 6)
    threshold_ms = $MaxOffsetMs
    windows_time_source = "$NtpServer,0x8"
}
$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$tempPath = "$OutputPath.tmp-$PID"
$report | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 -Path $tempPath
Move-Item -Force -Path $tempPath -Destination $OutputPath
Write-Output ($report | ConvertTo-Json -Depth 4)
if ($status -ne "pass") { exit 1 }
