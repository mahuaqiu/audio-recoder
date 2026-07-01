# FSK 时间标记功能设计

## 概述

在录音文件开头的静音区嵌入不可闻的 FSK（频移键控）时间标记，编码当天毫秒级时间戳。用于两台电脑同时录制时对比延迟。通过命令行参数 `--timestamp-mark` 启用，不影响默认录制行为。

## 编码格式

### 参数

| 参数 | 值 | 说明 |
|------|-----|------|
| 频率 '0' | 7000 Hz | FSK 低频 |
| 频率 '1' | 7500 Hz | FSK 高频 |
| 每比特时长 | 20 ms | 320 样本 @16kHz |
| 幅值 | -60 dBFS (0.001) | 远低于噪声底，不可闻 |
| 前导码 | 8 bit 交替 `10101010` | 用于同步检测 |
| 数据 | 27 bit | 一天内毫秒偏移量 |
| 保护间隔 | 0.2s 静音 | 标记与正式音频之间 |

### 总占用时长

```
0.16s (前导码) + 0.54s (27bit × 20ms) + 0.2s (保护) = 0.9s
```

### 27 bit 毫秒偏移量

```
值 = (小时 × 3600 + 分钟 × 60 + 秒) × 1000 + 毫秒
最大值 = 86399999 < 2^27 (134217728)
```

### 采样率要求

FSK 信号使用 7000Hz/7500Hz，奈奎斯特定理要求采样率 > 15000Hz 才能无混叠表示。

- 采样率 < 16kHz：直接报错拒绝启用标记功能
- 采样率 ≥ 16kHz：正常工作（7500Hz 距奈奎斯特极限 8000Hz 有 500Hz 余量）
- 采样率 ≥ 44.1kHz：未来可切换到超声波频段（19kHz/19.5kHz），当前实现不包含此优化

### 量化影响分析

FSK 信号幅值 0.001 在不同格式下的量化表现：

| 格式 | 转换公式 | 量化值 | 实际 dBFS | 可解码性 |
|------|---------|--------|----------|---------|
| S16 | 0.001 × 32767 ≈ 33 | ±33 LSB | ≈ -60 dBFS | ✅ 可靠 |
| S32 | 0.001 × 2147483647 ≈ 2147483 | ±2M LSB | ≈ -60 dBFS | ✅ 可靠 |
| F32 | 0.001 直接表示 | 0.001 | -60 dBFS | ✅ 完美 |

S16 下 ±33 LSB 的正弦波，Goertzel 在 320 样本窗口内可可靠检测（窄带 SNR 远高于 0dB）。

## 命令行参数

新增参数：

```
--timestamp-mark, -t    启用 FSK 时间标记（默认关闭）
```

示例：

```bash
audio-recoder -s microphone -r 16000 -d 60 -o meeting.wav --timestamp-mark
```

注意：`-t` 当前未被其他参数占用。若采样率 < 16kHz，该参数会报错拒绝。

## 模块结构

新增 `src/fsk_marker.rs` 模块：

| 函数/常量 | 职责 |
|-----------|------|
| `encode_timestamp(millis: u32, target_sample_rate: u32) -> Vec<f64>` | 将毫秒偏移量编码为 FSK 信号样本（按目标采样率生成） |
| `decode_timestamp(samples: &[f64], sample_rate: u32) -> Option<u32>` | 从样本中解码时间戳 |
| `decode_from_wav(path: &Path) -> Option<u32>` | 便捷解码：读取 WAV 文件并解码时间戳 |
| `goertzel(samples: &[f64], target_freq: f64, sample_rate: u32) -> f64` | Goertzel 算法核心（内部函数） |
| `MARK_DURATION_SECS: f64` | 标记总时长常量（≈0.9s） |

## 数据流

```
采集回调 → mpsc channel → wav_writer_loop
                              ↓ (若 timestamp_mark=true)
                            先写 FSK 标记样本（按 target_rate 生成，不经 resample）
                              ↓
                            再写正常音频样本 → WAV 文件
```

**重要**：FSK 标记样本按 `target_rate` 生成并直接写入 WAV，不经过 `resample()` 步骤。原因是线性插值重采样会严重失真高频正弦波（7000Hz/7500Hz 在 16kHz 下已接近奈奎斯特极限），而标记本身就是按目标采样率精确生成的，无需重采样。

## 集成变更

### 新增依赖

Cargo.toml 添加 `chrono = "0.4"` 用于获取本地时间和计算当天毫秒偏移量。

### RecordConfig

新增字段：

```rust
pub struct RecordConfig {
    // ... 现有字段
    pub timestamp_mark: bool,
}
```

### main.rs 变更点

1. **parse_args()** — 新增 `--timestamp-mark` / `-t` 参数解析
2. **run()** — 采集启动后，验证采样率并获取时间戳：
   ```rust
   let mark_millis = if config.timestamp_mark {
       if config.sample_rate < 16000 {
           return Err("时间标记需要采样率 >= 16000Hz".to_string());
       }
       let now = Local::now();
       let secs = now.num_seconds_from_midnight();
       let ms = now.timestamp_subsec_millis();
       Some((secs as u32 * 1000 + ms as u32))
   } else {
       None
   };
   ```
3. **wav_writer_loop()** — 新增 `mark_millis: Option<u32>` 参数，进入 recv 循环前写入标记样本：
   ```rust
   if let Some(millis) = mark_millis {
       let marker = fsk_marker::encode_timestamp(millis, target_rate);
       // 按 target_fmt 写入 marker 样本到 WAV...
   }
   // 然后进入正常的 rx.recv() 循环
   ```

### 保护间隔与采集数据竞争

采集在 `record_microphone()`/`record_speaker()` 返回后立即开始向 mpsc channel 发送数据。FSK 标记在 writer 线程的 recv 循环前写入。这意味着 writer 写完 0.9s 标记后，channel 中可能已积压了约 0.9s 的真实音频数据。

这**不是问题**：积压数据会立即被 writer 的 recv 循环消费。FSK 标记只是延迟了真实音频数据的写入，不影响音频内容完整性。0.2s 保护间隔保证 FSK 信号最后部分与真实音频之间有足够间隔，即使真实音频紧随标记之后到达。

### 不变的部分

- 采集层（microphone.rs / wasapi_loopback.rs / macos_speaker.rs）零修改
- 不带 `--timestamp-mark` 时行为完全不变
- WAV 文件格式不变（标准 PCM WAV）

## 核心算法

### 编码算法 (encode_timestamp)

1. 计算 27bit 二进制: millis 的 bit[26..0]
2. 生成前导码: 8 个 20ms 窗口，交替 7500Hz/7000Hz 正弦波
3. 生成数据: 27 个 20ms 窗口，bit=1 → 7500Hz，bit=0 → 7000Hz
4. 生成保护间隔: 0.2s 全零样本
5. 拼接: 前导码 + 数据 + 保护间隔
6. 所有正弦波幅值 = 0.001 (-60dBFS)

### 解码算法 (decode_timestamp)

1. 对每个 20ms 窗口计算 goertzel(7000Hz) 和 goertzel(7500Hz) 的能量
2. bit = if energy_1 > energy_0 { 1 } else { 0 }
3. 前 8 bit 验证前导码：计算与 `10101010` 的汉明距离，距离 ≤ 1 则通过
4. 后 27 bit 拼接为 u32 → 毫秒偏移量
5. 验证 millis < 86400000
6. 返回 Some(millis) 或 None

### Goertzel 算法

对指定频率计算能量，无需完整 FFT：

```rust
fn goertzel(samples: &[f64], target_freq: f64, sample_rate: u32) -> f64 {
    let n = samples.len() as f64;
    let k = (n * target_freq / sample_rate as f64).round();
    let w = 2.0 * PI * k / n;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}
```

## 精度分析

- 时间戳获取时机：采集启动后、写入线程启动前
- 与第一帧音频的延迟：< 10ms（取决于音频缓冲区大小）
- 两台电脑对比时，系统性误差方向相同，对相对延迟差值影响更小
- Goertzel 频率分辨率：20ms 窗口 @16kHz = 50Hz，而 7000Hz/7500Hz 间距 500Hz，余量 10 倍

## 错误处理

| 场景 | 处理方式 |
|------|---------|
| 采样率 < 16kHz + 启用标记 | 报错拒绝，退出 |
| 解码时前导码汉明距离 > 1 | 返回 None |
| 解码时 millis ≥ 86400000 | 返回 None |
| WAV 文件太短（<1秒）| 解码返回 None |

## 解码使用方式

提供库函数 `fsk_marker::decode_timestamp(samples, sample_rate)`，并额外提供便捷函数 `fsk_marker::decode_from_wav(path)` 内部调用 hound 读取 WAV 并转换为 f64 后解码。用户可在 Rust 代码中调用这些函数。
