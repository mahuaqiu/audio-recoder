//! 读取并强制校验 v2 同步报告（仅 pre_sync）。

use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ValidatedTimeSyncReport {
    pub schema_version: u32,
    pub report_kind: String,
    pub status: String,
    pub ntp_server: String,
    pub checked_at_unix_ns: i64,
    pub max_abs_offset_ms: f64,
    pub median_offset_ms: Option<f64>,
    pub rtt_p50_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawReport {
    schema_version: u32,
    report_kind: String,
    status: String,
    ntp_server: Option<String>,
    checked_at_unix_ns: Option<i64>,
    max_abs_offset_ms: Option<f64>,
    #[serde(default)]
    median_offset_ms: Option<f64>,
    #[serde(default)]
    rtt_p50_ms: Option<f64>,
}

/// 校验录制前同步报告。`now_unix_ns` 为当前 UTC 纳秒。
pub fn load_and_validate_pre_sync(
    path: &Path,
    max_offset_ms: f64,
    max_age_secs: u64,
    now_unix_ns: i64,
) -> Result<ValidatedTimeSyncReport, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("读取时间同步报告失败: {e}"))?;
    let raw: RawReport =
        serde_json::from_str(&text).map_err(|e| format!("解析时间同步报告失败: {e}"))?;

    if raw.schema_version != 2 {
        return Err(format!(
            "时间同步报告 schema_version 必须为 2，当前为 {}",
            raw.schema_version
        ));
    }
    if raw.report_kind != "pre_sync" {
        return Err(format!(
            "录制前仅接受 report_kind=pre_sync，当前为 {}",
            raw.report_kind
        ));
    }
    if raw.status.to_lowercase() != "pass" {
        return Err(format!("时间同步报告状态不是 pass: {}", raw.status));
    }
    let ntp_server = raw
        .ntp_server
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "时间同步报告缺少 ntp_server".to_string())?;
    let max_abs = raw
        .max_abs_offset_ms
        .ok_or_else(|| "时间同步报告缺少 max_abs_offset_ms".to_string())?;
    if !max_abs.is_finite() {
        return Err("max_abs_offset_ms 不是有限数".into());
    }
    if max_abs > max_offset_ms {
        return Err(format!(
            "时间同步偏差 {max_abs:.3}ms 超过阈值 {max_offset_ms:.3}ms"
        ));
    }
    let checked_at = raw
        .checked_at_unix_ns
        .ok_or_else(|| "时间同步报告缺少 checked_at_unix_ns".to_string())?;
    if checked_at > now_unix_ns + 60_000_000_000 {
        return Err("时间同步报告 checked_at 超前本机时钟超过 60 秒".into());
    }
    if now_unix_ns >= checked_at {
        let age_secs = ((now_unix_ns - checked_at) as u128 / 1_000_000_000u128) as u64;
        if age_secs > max_age_secs {
            return Err(format!(
                "时间同步报告已过期：距今 {age_secs}s，超过有效期 {max_age_secs}s"
            ));
        }
    }

    Ok(ValidatedTimeSyncReport {
        schema_version: raw.schema_version,
        report_kind: raw.report_kind,
        status: raw.status,
        ntp_server,
        checked_at_unix_ns: checked_at,
        max_abs_offset_ms: max_abs,
        median_offset_ms: raw.median_offset_ms,
        rtt_p50_ms: raw.rtt_p50_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(json: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ts-report-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        path
    }

    #[test]
    fn 拒绝_schema_v1() {
        let path = write_temp(
            r#"{"schema_version":1,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":100,"max_abs_offset_ms":1.0}"#,
        );
        let err = load_and_validate_pre_sync(&path, 5.0, 600, 100).unwrap_err();
        assert!(err.contains("schema") || err.contains("版本") || err.contains("2"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 拒绝过期报告() {
        let path = write_temp(
            r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":0,"max_abs_offset_ms":1.0}"#,
        );
        let now = 601_i64 * 1_000_000_000;
        let err = load_and_validate_pre_sync(&path, 5.0, 600, now).unwrap_err();
        assert!(err.contains("过期") || err.contains("有效期"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 拒绝缺_max_abs_offset() {
        let path = write_temp(
            r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":100}"#,
        );
        let err = load_and_validate_pre_sync(&path, 5.0, 600, 100).unwrap_err();
        assert!(!err.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 接受合法_pre_sync() {
        let path = write_temp(
            r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"192.168.10.20","checked_at_unix_ns":1000000000,"max_abs_offset_ms":1.5,"median_offset_ms":1.0}"#,
        );
        let rep = load_and_validate_pre_sync(&path, 5.0, 600, 1_000_000_000).unwrap();
        assert_eq!(rep.ntp_server, "192.168.10.20");
        let _ = std::fs::remove_file(path);
    }
}
