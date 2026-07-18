//! v2 timing sidecar 结构与 WAV/sidecar 原子写出。

use super::qpc_utc::QpcUtcCalibration;
use super::sha256::sha256_hex;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct TimingSidecarV2 {
    pub schema_version: u32,
    pub clock_domain: &'static str,
    pub source: &'static str,
    pub wav_file: String,
    pub wav_sha256: String,
    pub sample_rate: u32,
    pub actual_device_sample_rate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub first_pcm_utc_unix_ns: i64,
    pub first_pcm_millis_of_day: u32,
    pub fsk_semantics: &'static str,
    pub fsk_prefix_samples: usize,
    pub recording_started_unix_ns: i64,
    pub recording_ended_unix_ns: i64,
    pub qpc_utc_calibrations: Vec<QpcUtcCalibration>,
    pub clock_jump_detected: bool,
    pub anchors: Vec<TimingAnchor>,
    pub discontinuities: Vec<Discontinuity>,
    pub time_sync: TimeSyncSidecarV2,
}

#[derive(Debug, Serialize)]
pub struct TimingAnchor {
    pub wav_sample_index: u64,
    pub device_position: u64,
    pub qpc_100ns: u64,
    pub utc_unix_ns: i64,
}

#[derive(Debug, Serialize)]
pub struct Discontinuity {
    pub wav_sample_index: u64,
    pub device_position: Option<u64>,
    pub flags: u32,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TimeSyncSidecarV2 {
    pub schema_version: u32,
    pub report_kind: String,
    pub server: String,
    pub checked_at_unix_ns: i64,
    pub status: String,
    pub max_abs_offset_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_offset_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_p50_ms: Option<f64>,
}

/// 将已写好的 WAV partial 与 sidecar 原子替换为最终文件。
/// `wav_partial` 必须已存在；成功后变为 `wav_path` 与 `wav_path.timing.json`。
pub fn write_wav_and_sidecar_atomic(
    wav_path: &Path,
    wav_partial: &Path,
    mut sidecar: TimingSidecarV2,
) -> Result<(), String> {
    if sidecar.clock_jump_detected {
        let _ = fs::remove_file(wav_partial);
        return Err("检测到墙钟跳变，不产出正式录音产物".into());
    }

    let wav_bytes = fs::read(wav_partial).map_err(|e| format!("读取 WAV partial 失败: {e}"))?;
    sidecar.wav_sha256 = sha256_hex(&wav_bytes);

    let timing_path_buf = PathBuf::from(format!("{}.timing.json", wav_path.display()));
    let timing_partial_buf =
        PathBuf::from(format!("{}.timing.json.partial", wav_path.display()));
    let timing_path = timing_path_buf.as_path();
    let timing_partial = timing_partial_buf.as_path();

    // 清理旧 sidecar，避免错误配对
    let _ = fs::remove_file(timing_path);

    let json = serde_json::to_vec_pretty(&sidecar).map_err(|e| format!("序列化 sidecar 失败: {e}"))?;
    if let Some(parent) = timing_partial.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    fs::write(timing_partial, &json).map_err(|e| {
        let _ = fs::remove_file(wav_partial);
        format!("写入 sidecar partial 失败: {e}")
    })?;

    fs::rename(timing_partial, timing_path).map_err(|e| {
        let _ = fs::remove_file(wav_partial);
        let _ = fs::remove_file(timing_partial);
        format!("替换 sidecar 失败: {e}")
    })?;

    fs::rename(wav_partial, wav_path).map_err(|e| {
        let _ = fs::remove_file(timing_path);
        let _ = fs::remove_file(wav_partial);
        format!("替换 WAV 失败: {e}")
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn jump_时不留下产物() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sidecar-test-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("out.wav");
        let partial = dir.join("out.wav.partial");
        fs::write(&partial, b"RIFF").unwrap();
        let sidecar = TimingSidecarV2 {
            schema_version: 2,
            clock_domain: "windows-utc-synchronized-by-ntp",
            source: "wasapi-loopback",
            wav_file: "out.wav".into(),
            wav_sha256: String::new(),
            sample_rate: 16000,
            actual_device_sample_rate: 48000,
            device_id: None,
            device_name: None,
            first_pcm_utc_unix_ns: 0,
            first_pcm_millis_of_day: 0,
            fsk_semantics: "first_pcm_sample",
            fsk_prefix_samples: 100,
            recording_started_unix_ns: 0,
            recording_ended_unix_ns: 1,
            qpc_utc_calibrations: vec![],
            clock_jump_detected: true,
            anchors: vec![],
            discontinuities: vec![],
            time_sync: TimeSyncSidecarV2 {
                schema_version: 2,
                report_kind: "pre_sync".into(),
                server: "1.1.1.1".into(),
                checked_at_unix_ns: 0,
                status: "pass".into(),
                max_abs_offset_ms: 1.0,
                median_offset_ms: None,
                rtt_p50_ms: None,
            },
        };
        let err = write_wav_and_sidecar_atomic(&wav, &partial, sidecar).unwrap_err();
        assert!(err.contains("跳变"));
        assert!(!wav.exists());
        let _ = fs::remove_dir_all(dir);
    }
}
