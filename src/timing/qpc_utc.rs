//! QPC 与 UTC 多点校准、跳变检测与 packet 时间映射。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct QpcUtcCalibration {
    pub phase: String,
    pub qpc_100ns: u64,
    pub utc_unix_ns: i64,
    pub span_qpc_100ns: u64,
}

#[derive(Debug, Default)]
pub struct QpcUtcMapper {
    calibrations: Vec<QpcUtcCalibration>,
    clock_jump_detected: bool,
}

impl QpcUtcMapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_for_test(&mut self, cal: QpcUtcCalibration) {
        self.detect_jump_against_last(&cal);
        self.calibrations.push(cal);
    }

    /// Windows 上采样一组校准点；非 Windows 返回错误。
    pub fn capture(&mut self, phase: &str) -> Result<&QpcUtcCalibration, String> {
        let sample = sample_once()?;
        let cal = QpcUtcCalibration {
            phase: phase.to_string(),
            qpc_100ns: sample.0,
            utc_unix_ns: sample.1,
            span_qpc_100ns: sample.2,
        };
        self.detect_jump_against_last(&cal);
        self.calibrations.push(cal);
        Ok(self.calibrations.last().unwrap())
    }

    pub fn map_qpc_to_utc_ns(&self, qpc_100ns: u64) -> Result<i64, String> {
        if self.calibrations.is_empty() {
            return Err("没有可用的 QPC/UTC 校准点".into());
        }
        if self.calibrations.len() == 1 {
            let c = &self.calibrations[0];
            let delta_ticks = qpc_100ns as i128 - c.qpc_100ns as i128;
            return i64::try_from(c.utc_unix_ns as i128 + delta_ticks * 100)
                .map_err(|_| "UTC 纳秒超出范围".into());
        }
        // 找包围 qpc 的最近两点；否则用两端外推
        let first = &self.calibrations[0];
        let last = self.calibrations.last().unwrap();
        if qpc_100ns <= first.qpc_100ns {
            return interpolate(first, &self.calibrations[1], qpc_100ns);
        }
        if qpc_100ns >= last.qpc_100ns {
            let n = self.calibrations.len();
            return interpolate(&self.calibrations[n - 2], last, qpc_100ns);
        }
        for window in self.calibrations.windows(2) {
            if qpc_100ns >= window[0].qpc_100ns && qpc_100ns <= window[1].qpc_100ns {
                return interpolate(&window[0], &window[1], qpc_100ns);
            }
        }
        interpolate(first, last, qpc_100ns)
    }

    pub fn clock_jump_detected(&self) -> bool {
        self.clock_jump_detected
    }

    pub fn calibrations(&self) -> &[QpcUtcCalibration] {
        &self.calibrations
    }

    fn detect_jump_against_last(&mut self, new: &QpcUtcCalibration) {
        let Some(old) = self.calibrations.last() else {
            return;
        };
        if new.qpc_100ns <= old.qpc_100ns {
            self.clock_jump_detected = true;
            return;
        }
        let dqpc = (new.qpc_100ns - old.qpc_100ns) as i128;
        let dutc = new.utc_unix_ns as i128 - old.utc_unix_ns as i128;
        let expected = dqpc * 100;
        let abs_err = (dutc - expected).abs();
        if abs_err > 5_000_000 {
            // > 5ms
            self.clock_jump_detected = true;
            return;
        }
        if expected > 0 {
            let ratio = dutc as f64 / expected as f64;
            if (ratio - 1.0).abs() > 50e-6 && abs_err > 1_000_000 {
                self.clock_jump_detected = true;
            }
        }
    }
}

fn interpolate(left: &QpcUtcCalibration, right: &QpcUtcCalibration, qpc: u64) -> Result<i64, String> {
    let dq = right.qpc_100ns as i128 - left.qpc_100ns as i128;
    if dq <= 0 {
        return Err("校准点 QPC 不单调".into());
    }
    let du = right.utc_unix_ns as i128 - left.utc_unix_ns as i128;
    let value = left.utc_unix_ns as i128 + (qpc as i128 - left.qpc_100ns as i128) * du / dq;
    i64::try_from(value).map_err(|_| "UTC 纳秒超出范围".into())
}

#[cfg(target_os = "windows")]
fn sample_once() -> Result<(u64, i64, u64), String> {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    let mut frequency = 0i64;
    unsafe {
        QueryPerformanceFrequency(&mut frequency)
            .map_err(|e| format!("获取 QPC 频率失败: {e}"))?;
    }
    if frequency <= 0 {
        return Err("无效 QPC 频率".into());
    }
    let mut best: Option<(i64, u64, i64)> = None; // span_ticks, mid_100ns, utc_ns
    for _ in 0..8 {
        let mut before = 0i64;
        let mut after = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut before).map_err(|e| format!("读取 QPC 失败: {e}"))?;
        }
        let filetime = unsafe { GetSystemTimePreciseAsFileTime() };
        unsafe {
            QueryPerformanceCounter(&mut after).map_err(|e| format!("读取 QPC 失败: {e}"))?;
        }
        let span = after - before;
        if span < 0 {
            continue;
        }
        if best.map(|(old, _, _)| span < old).unwrap_or(true) {
            let mid_ticks = (before as i128 + after as i128) / 2;
            let mid_100ns = (mid_ticks * 10_000_000i128 / frequency as i128) as u64;
            let ft =
                ((filetime.dwHighDateTime as u64) << 32 | filetime.dwLowDateTime as u64) as i128;
            let unix_100ns = ft - 116_444_736_000_000_000i128;
            let utc_ns = (unix_100ns * 100) as i64;
            best = Some((span, mid_100ns, utc_ns));
        }
    }
    let (span_ticks, mid_100ns, utc_ns) = best.ok_or("无法建立 QPC/UTC 映射")?;
    let span_100ns = (span_ticks as i128 * 10_000_000i128 / frequency as i128).max(0) as u64;
    Ok((mid_100ns, utc_ns, span_100ns))
}

#[cfg(not(target_os = "windows"))]
fn sample_once() -> Result<(u64, i64, u64), String> {
    Err("当前平台没有 WASAPI QPC 到 UTC 映射".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 线性映射两点() {
        let mut m = QpcUtcMapper::new();
        m.push_for_test(QpcUtcCalibration {
            phase: "start".into(),
            qpc_100ns: 1_000,
            utc_unix_ns: 1_000_000,
            span_qpc_100ns: 1,
        });
        m.push_for_test(QpcUtcCalibration {
            phase: "end".into(),
            qpc_100ns: 2_000,
            utc_unix_ns: 1_000_000 + 100_000,
            span_qpc_100ns: 1,
        });
        let utc = m.map_qpc_to_utc_ns(1_500).unwrap();
        assert_eq!(utc, 1_000_000 + 50_000);
    }

    #[test]
    fn 大跳变标记_clock_jump() {
        let mut m = QpcUtcMapper::new();
        m.push_for_test(QpcUtcCalibration {
            phase: "start".into(),
            qpc_100ns: 1_000,
            utc_unix_ns: 0,
            span_qpc_100ns: 1,
        });
        m.push_for_test(QpcUtcCalibration {
            phase: "periodic".into(),
            qpc_100ns: 1_100,
            utc_unix_ns: 20_000_000,
            span_qpc_100ns: 1,
        });
        assert!(m.clock_jump_detected());
    }
}
