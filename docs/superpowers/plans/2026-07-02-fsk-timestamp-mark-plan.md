# FSK 时间标记功能实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在录音文件开头嵌入不可闻的 FSK 时间标记，编码当天毫秒级时间戳。通过命令行参数 `--timestamp-mark` 启用，用于两台电脑同时录制时对比延迟。

**Architecture:** 
- 新增 `src/fsk_marker.rs` 模块实现 FSK 编码/解码
- 编码：在 WAV 写入前先写入 0.9s FSK 标记（7000Hz/7500Hz FSK，27bit 毫秒时间戳）
- 解码：Goertzel 算法检测频率，解码时间戳
- 集成：修改 main.rs 的参数解析和写入流程

**Tech Stack:** Rust, chrono (新增), hound (已有)

---

## 文件结构

```
src/
├── main.rs              # 修改: parse_args, run, wav_writer_loop
├── fsk_marker.rs        # 新建: FSK 编码/解码模块
└── capture/
    └── mod.rs           # 修改: RecordConfig 添加 timestamp_mark 字段
```

### Cargo.toml
- 新增: `chrono = "0.4"`

---

## 实现计划

### 任务 1: 添加 chrono 依赖

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 在 Cargo.toml 的 [dependencies] 段添加 chrono**

```toml
[dependencies]
# ... 现有依赖 ...
chrono = "0.4"
```

- [ ] **Step 2: 运行 cargo check 验证依赖**

```
cd D:\code\audio-recoder
cargo check
```
Expected: 编译成功（可能有 unused warnings 没关系）

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "deps: 添加 chrono 依赖用于时间标记"
```

---

### 任务 2: 创建 fsk_marker 模块

**Files:**
- Create: `src/fsk_marker.rs`

- [ ] **Step 1: 创建 fsk_marker.rs 模块文件**

```rust
//! FSK 时间标记编码/解码模块
//! 
//! 在录音文件开头的静音区嵌入 FSK 时间标记，编码当天毫秒级时间戳。

use std::f64::consts::PI;
use std::path::Path;
use std::fs::File;
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
    
    #[test]
    fn test_decode_with_noise() {
        let sample_rate = 16000;
        let millis = 50000000;
        
        let mut encoded = encode_timestamp(millis, sample_rate);
        
        // 添加低幅值噪声（-70dBFS ≈ 0.0003）
        for sample in &mut encoded {
            *sample += (rand_simple() - 0.5) * 0.0006;
        }
        
        let decoded = decode_timestamp(&encoded, sample_rate);
        assert_eq!(decoded, Some(millis));
    }
    
    // 简单的伪随机数生成器（用于测试）
    fn rand_simple() -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        (nanos as f64 % 1000.0) / 1000.0
    }
}
```

注意：上述测试中 `rand_simple()` 可以直接使用 `fastrand` crate 或标准库的 `rand` crate。为了简化，可以删除带噪声的测试或使用固定噪声模式。

- [ ] **Step 2: 运行 cargo check 验证模块语法**

```
cd D:\code\audio-recoder
cargo check
```
Expected: 编译成功

- [ ] **Step 3: 运行测试验证功能**

```
cargo test fsk_marker
```
Expected: 4-5 tests passed

- [ ] **Step 4: Commit**

```bash
git add src/fsk_marker.rs
git commit -m "feat: 添加 FSK 时间标记编码/解码模块"
```

---

### 任务 3: 修改 RecordConfig 添加字段

**Files:**
- Modify: `src/capture/mod.rs:180-200`

- [ ] **Step 1: 在 RecordConfig 结构体中添加 timestamp_mark 字段**

找到 `pub struct RecordConfig {` 定义，添加新字段：

```rust
pub struct RecordConfig {
    pub source: Source,
    pub sample_rate: u32,
    pub sample_fmt: SampleFmt,
    pub duration_secs: u64,
    pub output_path: String,
    /// 设备名称（模糊匹配），None 表示使用默认设备
    pub device_name: Option<String>,
    /// 是否前台阻塞模式，false 则后台运��（默认）
    pub foreground: bool,
    pub timestamp_mark: bool,  // 新增
}
```

- [ ] **Step 2: 更新 Default 实现**

找到 `impl Default for RecordConfig {`，添加默认值：

```rust
impl Default for RecordConfig {
    fn default() -> Self {
        RecordConfig {
            source: Source::Microphone,
            sample_rate: 16000,
            sample_fmt: SampleFmt::S16,
            duration_secs: 120,
            output_path: "recording.wav".to_string(),
            device_name: None,
            foreground: false,
            timestamp_mark: false,  // 新增
        }
    }
}
```

- [ ] **Step 3: 运行 cargo check 验证**

```
cargo check
```
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add src/capture/mod.rs
git commit -m "feat: RecordConfig 添加 timestamp_mark 字段"
```

---

### 任务 4: 修改 parse_args 添加命令行参数

**Files:**
- Modify: `src/main.rs:415-485`

- [ ] **Step 1: 在 parse_args 函数中添加 --timestamp-mark 参数解析**

在 `Short('b') | Long("blocking") => {` 之后、`Short('h') | Long("help")` 之前添加：

```rust
            Short('b') | Long("blocking") => {
                config.foreground = true;
            }
            Short('t') | Long("timestamp-mark") => {
                config.timestamp_mark = true;
            }
            Short('h') | Long("help") => {
```

- [ ] **Step 2: 运行 cargo check 验证**

```
cargo check
```
Expected: 编译成功

- [ ] **Step 3: 运行 --help 验证参数显示**

```
cargo run -- --help
```
Expected: 输出包含 `--timestamp-mark, -t`

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: 添加 --timestamp-mark 命令行参数"
```

---

### 任务 5: 修改 run() 获取时间戳

**Files:**
- Modify: `src/main.rs:106-180`

- [ ] **Step 1: 在 run() 函数中添加时间戳获取逻辑**

在 `run()` 函数中，找到启动写入线程的位置（`let writer_thread = std::thread::spawn` 之前），添加时间戳获取：

```rust
    // ... 现有代码（采集启动）...
    
    // 获取时间戳（如果启用）
    let mark_millis = if config.timestamp_mark {
        if config.sample_rate < 16000 {
            return Err("时间标记需要采样率 >= 16000Hz".to_string());
        }
        use chrono::Local;
        let now = Local::now();
        let secs = now.num_seconds_from_midnight();
        let ms = now.timestamp_subsec_millis();
        Some((secs as u32 * 1000 + ms as u32))
    } else {
        None
    };
    
    let writer_thread = std::thread::spawn(move || {
        wav_writer_loop(
            rx,
            &output_path,
            actual_sample_rate,
            target_sample_rate,
            target_sample_fmt,
            mark_millis,
        )
    });
```

- [ ] **Step 2: 运行 cargo check 验证**

```
cargo check
```
Expected: 编译成功

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: run() 添加时间戳获取逻辑"
```

---

### 任务 6: 修改 wav_writer_loop 写入标记

**Files:**
- Modify: `src/main.rs:307-365`

- [ ] **Step 1: 修改 wav_writer_loop 函数签名**

在函数签名中添加 `mark_millis` 参数：

```rust
fn wav_writer_loop(
    rx: mpsc::Receiver<Vec<f64>>,
    output_path: &str,
    actual_rate: u32,
    target_rate: u32,
    target_fmt: SampleFmt,
    mark_millis: Option<u32>,  // 新增参数
) -> Result<(), String> {
```

- [ ] **Step 2: 在写入循环前添加标记写入逻辑**

在 `let mut writer = ...` 成功后、`while let Ok(samples) = rx.recv()` 之前添加：

```rust
    let mut writer = hound::WavWriter::create(output_path, spec)
        .map_err(|e| format!("创建 WAV 文件失败: {e}"))?;
    
    // 如果启用了时间标记，先写入标记样本
    if let Some(millis) = mark_millis {
        let marker_samples = fsk_marker::encode_timestamp(millis, target_rate);
        
        for &sample in &marker_samples {
            match target_fmt {
                SampleFmt::S16 => {
                    let s = (sample * 32767.0).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    writer.write_sample(s).map_err(|e| format!("写入标记失败: {e}"))?;
                }
                SampleFmt::S32 => {
                    let s = (sample * 2147483647.0).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                    writer.write_sample(s).map_err(|e| format!("写入标记失败: {e}"))?;
                }
                SampleFmt::F32 => {
                    let s = sample as f32;
                    writer.write_sample(s).map_err(|e| format!("写入标记失败: {e}"))?;
                }
            }
        }
    }
    
    while let Ok(samples) = rx.recv() {
```

注意：需要在 main.rs 顶部声明模块：

```rust
mod fsk_marker;
```

- [ ] **Step 3: 运行 cargo check 验证**

```
cargo check
```
Expected: 编译成功

- [ ] **Step 4: 运行功能测试**

如果系统有麦克风，可以测试：

```
cargo run -- -s microphone -r 16000 -d 5 -o test_mark.wav --timestamp-mark
```

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: wav_writer_loop 添加 FSK 标记写入功能"
```

---

### 任务 7: 端到端功能验证

- [ ] **Step 1: 运行单元测试**

```
cargo test fsk_marker
```
Expected: 所有测试通过

- [ ] **Step 2: 验证不带标记时行为不变**

编译确认不带 --timestamp-mark 时代码行为不变。

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: 完成 FSK 时间标记功能端到端验证"
```

---

## 总结

完成以上 7 个任务后，FSK 时间标记功能即可投入使用：

1. 添加 chrono 依赖
2. 创建 fsk_marker.rs 模块（含编码、解码、Goertzel 算法）
3. 修改 RecordConfig 添加字段（更新结构和 Default 实现）
4. 修改 parse_args 添加 -t/--timestamp-mark 参数
5. 修改 run() 获取当前时间戳
6. 修改 wav_writer_loop 写入标记样本
7. 端到端功能验证

**预计总改动：**
- 新增文件：1 个 (fsk_marker.rs)
- 修改文件：3 个 (Cargo.toml, capture/mod.rs, main.rs)
- 新增代码：约 250 行
- 单元测试：5 个