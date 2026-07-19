[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateNotNullOrEmpty()]
    [string]$NtpServer,
    [int]$Samples = 10,
    [double]$MaxOffsetMs = 5.0,
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $PSScriptRoot "time-sync-report.json"
}

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

. (Join-Path $PSScriptRoot "Parse-Stripchart.ps1")

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

try {
    $offsets = Get-OffsetMillisecondsFromStripchart -Lines @($raw)
    $report = New-TimeSyncReportObject `
        -ReportKind pre_sync `
        -NtpServer $NtpServer `
        -SampleCount $Samples `
        -OffsetsMs $offsets `
        -ThresholdMs $MaxOffsetMs
}
catch {
    Fail $_.Exception.Message
}

Write-TimeSyncReportJson -Report $report -OutputPath $OutputPath
Write-Output ($report | ConvertTo-Json -Depth 4)
if ($report.status -ne "pass") { exit 1 }
