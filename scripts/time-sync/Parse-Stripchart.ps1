# Shared helpers for w32tm stripchart parsing and v2 time-sync reports.

function Get-OffsetMillisecondsFromStripchart {
    param([string[]]$Lines)
    $values = @()
    foreach ($line in $Lines) {
        $match = [regex]::Match([string]$line, '([+-]?\d+(?:\.\d+)?)s')
        if ($match.Success) {
            $ms = [double]$match.Groups[1].Value * 1000.0
            if (-not [double]::IsNaN($ms) -and -not [double]::IsInfinity($ms)) {
                $values += $ms
            }
        }
    }
    return ,$values
}

function New-TimeSyncReportObject {
    param(
        [ValidateSet('pre_sync', 'post_verify')]
        [string]$ReportKind,
        [string]$NtpServer,
        [int]$SampleCount,
        [double[]]$OffsetsMs,
        [double]$ThresholdMs
    )
    if ($OffsetsMs.Count -lt 5) {
        throw ("insufficient offset samples: {0}" -f $OffsetsMs.Count)
    }
    $sorted = @($OffsetsMs | Sort-Object)
    if ($sorted.Count % 2 -eq 1) {
        $median = $sorted[[int]($sorted.Count / 2)]
    }
    else {
        $median = ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2.0
    }
    $maxAbs = ($OffsetsMs | ForEach-Object { [math]::Abs($_) } | Measure-Object -Maximum).Maximum
    if ($maxAbs -le $ThresholdMs) {
        $status = 'pass'
    }
    else {
        $status = 'fail'
    }
    $nowNs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000000L
    $source = '{0},0x8' -f $NtpServer
    $report = [ordered]@{}
    $report['schema_version'] = 2
    $report['report_kind'] = $ReportKind
    $report['status'] = $status
    $report['computer_name'] = $env:COMPUTERNAME
    $report['ntp_server'] = $NtpServer
    $report['checked_at_unix_ns'] = $nowNs
    $report['sample_count'] = $SampleCount
    $report['valid_sample_count'] = $OffsetsMs.Count
    $report['median_offset_ms'] = [math]::Round($median, 6)
    $report['max_abs_offset_ms'] = [math]::Round($maxAbs, 6)
    $report['rtt_p50_ms'] = $null
    $report['threshold_ms'] = $ThresholdMs
    $report['windows_time_source'] = $source
    return $report
}

function Write-TimeSyncReportJson {
    param(
        [System.Collections.IDictionary]$Report,
        [string]$OutputPath
    )
    $parent = Split-Path -Parent $OutputPath
    if ($parent) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    $tempPath = ('{0}.tmp-{1}' -f $OutputPath, $PID)
    $Report | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 -Path $tempPath
    Move-Item -Force -Path $tempPath -Destination $OutputPath
}