[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$NtpServer,
    [int]$Samples = 10,
    [double]$MaxOffsetMs = 5.0,
    [string]$OutputPath = ".\time-sync-verify-report.json"
)

$script = Join-Path $PSScriptRoot "sync-windows-time.ps1"
& $script -NtpServer $NtpServer -Samples $Samples -MaxOffsetMs $MaxOffsetMs -OutputPath $OutputPath
exit $LASTEXITCODE
