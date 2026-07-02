//! FSK 时间标记编码/解码模块
//!
//! 在录音文件开头的静音区嵌入 FSK 时间标记，编码当天毫秒级时间戳。

use std::f64::consts::PI;
use std::path::Path;
use hound::{WavReader, SampleFormat};

// === 常量 ===

/// 标记总时长（秒），约 0.9s
pub const MARK_DURATION_SECS: f64 = 0.9;

/// FSK 参数
pub const FSK_FREQ_0: f64 = 7000.0;   // '0' 对应频率
pub const FSK_FREQ_1: f64 = 7500.0;   // '1' 对应频率
pub const SYMBOL_DURATION_SECS: f64 = 0.020; // 每比特 20ms
pub const PREAMBLE_BITS: usize = 8;   // 前导码 bit 数
pub const DATA_BITS: usize = 27;      // 数据 bit 数
pub const GUARD_DURATION: f64 = 0.2;  // 保护间隔（秒）
pub const MARKER_AMPLITUDE: f64 = 0.001; // -60dBFS

// === 编码 ===

/// 将毫秒偏移量编码为 FSK 信号样本
///
/// # Arguments
/// * `millis` - 当天毫秒偏移量 (0 ~ 86399999)
/// * `sample_rate` - 目标采样率
///
/// # Returns
/// 编码后的 f64 样本向量
pub fn encode_timestamp(millis: u32, sample_rate: u32) -> Vec<f64> {
    let mut samples = Vec::new();

    // 计算每个符号的样本数
    let bits_per_window = (SYMBOL_DURATION_SECS * sample_rate as f64) as usize;

    // 生成前导码 (10101010...)
    for i in 0..PREAMBLE_BITS {
        let freq = if i % 2 == 0 { FSK_FREQ_1 } else { FSK_FREQ_0 };
        samples.extend(generate_sine(freq, bits_per_window, sample_rate));
    }

    // 生成数据位
    for i in 0..DATA_BITS {
        let bit = (millis >> (DATA_BITS - 1 - i)) & 1;
        let freq = if bit == 1 { FSK_FREQ_1 } else { FSK_FREQ_0 };
        samples.extend(generate_sine(freq, bits_per_window, sample_rate));
    }

    // 生成保护间隔 (静音)
    let guard_samples = (GUARD_DURATION * sample_rate as f64) as usize;
    samples.extend(vec![0.0; guard_samples]);

    samples
}

/// 生成正弦波样本
fn generate_sine(freq: f64, num_samples: usize, sample_rate: u32) -> Vec<f64> {
    let mut samples = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f64 / sample_rate as f64;
        let sample = MARKER_AMPLITUDE * (2.0 * PI * freq * t).sin();
        samples.push(sample);
    }
    samples
}

// === Goertzel 算法 ===

/// Goertzel 算法计算指定频率的能量
fn goertzel(samples: &[f64], target_freq: f64, sample_rate: u32) -> f64 {
    // 防御性检查：空输入返回 0.0
    if samples.is_empty() {
        return 0.0;
    }

    let n = samples.len() as f64;
    let k = (n * target_freq / sample_rate as f64).round();
    let w = 2.0 * PI * k / n;
    let coeff = 2.0 * w.cos();

    let mut s1 = 0.0;
    let mut s2 = 0.0;

    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }

    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

// === 解码 ===

/// 从样本中解码时间戳
///
/// # Arguments
/// * `samples` - f64 音频样本
/// * `sample_rate` - 采样率
///
/// # Returns
/// 成功返回 Some(毫秒偏移量)，失败返回 None
pub fn decode_timestamp(samples: &[f64], sample_rate: u32) -> Option<u32> {
    let bits_per_window = (SYMBOL_DURATION_SECS * sample_rate as f64) as usize;
    let total_bits = PREAMBLE_BITS + DATA_BITS;

    let mut bits = Vec::with_capacity(total_bits);

    for i in 0..total_bits {
        let idx = i * bits_per_window;
        if idx + bits_per_window > samples.len() {
            return None;
        }

        let window = &samples[idx..idx + bits_per_window];
        let energy_0 = goertzel(window, FSK_FREQ_0, sample_rate);
        let energy_1 = goertzel(window, FSK_FREQ_1, sample_rate);

        bits.push(if energy_1 > energy_0 { 1 } else { 0 });
    }

    // 验证前导码 (10101010)，允许汉明距离 ≤ 1
    let mut hamming_distance = 0;
    for i in 0..PREAMBLE_BITS {
        let expected = if i % 2 == 0 { 1 } else { 0 }; // 10101010
        if bits[i] != expected {
            hamming_distance += 1;
        }
    }

    if hamming_distance > 1 {
        return None;
    }

    // 解析数据位
    let mut millis = 0u32;
    for i in 0..DATA_BITS {
        millis = (millis << 1) | bits[PREAMBLE_BITS + i];
    }

    // 验证范围: millis < 86400000 (一天毫秒数)
    if millis >= 86400000 {
        return None;
    }

    Some(millis)
}

/// 便捷解码：从 WAV 文件读取并解码时间戳
///
/// # Arguments
/// * `path` - WAV 文件路径
///
/// # Returns
/// 成功返回 Some(毫秒偏移量)，失败返回 None
pub fn decode_from_wav(path: &Path) -> Option<u32> {
    let mut reader = WavReader::open(path).ok()?;

    let spec = reader.spec();
    let sample_rate = spec.sample_rate;

    // 只支持单声道 WAV 文件
    if spec.channels != 1 {
        eprintln!("警告: decode_from_wav 只支持单声道 WAV，当前文件为 {} 声道", spec.channels);
        return None;
    }

    // 读取样本并转换为 f64
    let samples: Vec<f64> = match spec.sample_format {
        SampleFormat::Int => {
            match spec.bits_per_sample {
                16 => reader.samples::<i16>()
                    .filter_map(|s| s.ok())
                    .map(|s| s as f64 / 32768.0)
                    .collect(),
                32 => reader.samples::<i32>()
                    .filter_map(|s| s.ok())
                    .map(|s| s as f64 / 2147483648.0)
                    .collect(),
                _ => return None,
            }
        }
        SampleFormat::Float => {
            reader.samples::<f32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f64)
                .collect()
        }
    };

    // 至少需要 1 秒的样本
    if samples.len() < sample_rate as usize {
        return None;
    }

    decode_timestamp(&samples, sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let sample_rate = 16000;
        let millis = 38250001; // 10:30:50.001

        let encoded = encode_timestamp(millis, sample_rate);

        // 验证长度（约 0.9s @ 16kHz = 14400 样本）
        let expected_len = ((PREAMBLE_BITS + DATA_BITS) as f64 * SYMBOL_DURATION_SECS
            + GUARD_DURATION) * sample_rate as f64;
        assert!((encoded.len() as f64 - expected_len).abs() < 10.0);

        // 解码验证
        let decoded = decode_timestamp(&encoded, sample_rate);
        assert_eq!(decoded, Some(millis));
    }

    #[test]
    fn test_encode_decode_various_times() {
        let sample_rate = 16000;
        let test_cases = [0, 1000, 3600000, 86399999];

        for millis in test_cases {
            let encoded = encode_timestamp(millis, sample_rate);
            let decoded = decode_timestamp(&encoded, sample_rate);
            assert_eq!(decoded, Some(millis), "Failed for millis={}", millis);
        }
    }

    #[test]
    fn test_encode_decode_44100() {
        let sample_rate = 44100;
        let millis = 12345678;

        let encoded = encode_timestamp(millis, sample_rate);
        let decoded = decode_timestamp(&encoded, sample_rate);
        assert_eq!(decoded, Some(millis));
    }

    #[test]
    fn test_decode_rejects_invalid_millis() {
        let sample_rate = 16000;
        let millis = 86400000; // 超出范围

        let encoded = encode_timestamp(millis, sample_rate);
        let decoded = decode_timestamp(&encoded, sample_rate);
        assert_eq!(decoded, None);
    }
}
