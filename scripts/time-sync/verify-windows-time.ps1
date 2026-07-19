[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$NtpServer,
    [int]$Samples = 10,
    [double]$MaxOffsetMs = 5.0,
    [string]$OutputPath = ".\time-sync-verify-report.json"
)

$ErrorActionPreference = "Stop"

# 只读复检：禁止执行任何时间配置、服务重启或主动同步操作。

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

. (Join-Path $PSScriptRoot "Parse-Stripchart.ps1")

if ($Samples -lt 5 -or $MaxOffsetMs -le 0) {
    Fail "Samples 至少为 5，MaxOffsetMs 必须大于 0"
}

# 仅查询当前状态，不修改配置
& w32tm /query /source 2>&1 | Out-Null
& w32tm /query /status 2>&1 | Out-Null

$raw = & w32tm /stripchart "/computer:$NtpServer" "/samples:$Samples" /dataonly 2>&1
if ($LASTEXITCODE -ne 0) {
    Fail "无法从 NTP server 获取 stripchart 样本（只读复检）`n$($raw -join "`n")"
}

try {
    $offsets = Get-OffsetMillisecondsFromStripchart -Lines @($raw)
    $report = New-TimeSyncReportObject `
        -ReportKind post_verify `
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
