# 音频录制命令行工具 — 设计文档

## 概述

一个 Rust 命令行音频录制工具，支持录制麦克风和扬声器音频，输出 WAV 格式。支持 Windows 和 macOS 交叉编译，打包体积目标 < 5MB。

## 架构

```
main.rs
├── cli        — 命令行参数解析 (lexopt)
├── capture
│   ├── mod.rs           — 录制调度，根据 source 类型分发
│   ├── microphone.rs    — 麦克风录制 (cpal)
│   ├── wasapi_loopback.rs   — Windows 扬声器录制 (WASAPI loopback)
│   └── screencapturekit.rs  — macOS 扬声器录制 (ScreenCaptureKit)
└── wav_writer — WAV 写入 (hound)
```

## 命令行接口

```
audio-recorder [OPTIONS]

选项:
  -s, --source <SOURCE>     音频源: microphone | speaker (默认: microphone)
  -r, --sample-rate <RATE>  采样率 (默认: 16000)
  -f, --sample-fmt <FMT>    采样格式: s16 | s32 | f32 (默认: s16)
  -d, --duration <SECS>     录制时长秒数 (默认: 120)
  -o, --output <PATH>       输出文件路径 (默认: recording.wav)
```

示例：
```bash
# 录制麦克风，默认参数
audio-recorder

# 录制扬声器，48kHz，f32 格式，输出到指定路径
audio-recorder -s speaker -r 48000 -f f32 -d 60 -o /tmp/output.wav
```

## 模块设计

### 1. CLI 解析 (lexopt)

轻量级参数解析，解析上述参数。无效参数时打印 usage 退出。

### 2. 麦克风录制 (cpal)

- 使用 `cpal` 获取默认输入设备
- 创建输入流，按配置的采样率和格式采集音频
- 将采样数据通过 channel 发送给 WAV 写入线程
- 录制时长到达后停止流

采样格式映射：
| CLI 参数 | cpal SampleFormat |
|----------|-------------------|
| s16      | I16               |
| s32      | I32               |
| f32      | F32               |

如果设备不支持请求的采样率/格式，选择最接近的支持值并提示用户。

### 3. Windows 扬声器录制 (WASAPI loopback)

- 使用 `windows` crate 调用 WASAPI API
- 以 eRender + eConsole 模式打开默认输出设备的 loopback 流
- 捕获扬声器回放数据，通过 channel 发送给 WAV 写入线程
- 录制时长到达后停止

条件编译：`#[cfg(target_os = "windows")]`

### 4. macOS 扬声器录制 (ScreenCaptureKit)

- 使用 `objc2` + `block2` crate 调用 ScreenCaptureKit 的 Objective-C API
- 通过 `SCStreamConfiguration` 配置音频捕获（不含视频）
- 从 `SCStream` 的 sampleBuffer 回调中提取 CMSampleBuffer → AudioBufferList → PCM 数据
- 通过 channel 发送给 WAV 写入线程
- 需要用户授予屏幕录制权限（系统弹窗提示）

条件编译：`#[cfg(target_os = "macos")]`

### 5. WAV 写入 (hound)

- 在独立线程中运行
- 从 channel 接收采样数据，用 `hound::WavWriter` 追加写入
- 支持配置的采样率、通道数（单声道）、采样格式
- 录制结束后自动 flush 并关闭文件

## 数据流

```
音频源 (cpal / WASAPI / SCK)
    │
    ▼
采集回调 → mpsc::Sender
    │
    ▼
WAV 写入线程 ← mpsc::Receiver
    │
    ▼
hound::WavWriter → .wav 文件
```

- 使用 `std::sync::mpsc` channel，缓冲区大小 4096 个采样
- 所有平台统一将采样数据转为 f64 传给 hound（hound 内部会按目标格式写入）

## 错误处理

- 设备不可用：打印错误信息，退出码 1
- 权限不足（macOS 屏幕录制权限）：提示用户去系统设置授权
- 磁盘写入失败：打印 IO 错误，退出码 1
- Ctrl+C 中断：捕获 SIGINT，将已录制数据 flush 到文件后退出

## 交叉编译与打包

### Cargo.toml 关键配置

```toml
[profile.release]
opt-level = "z"       # 最小体积优化
lto = true            # 链接时优化
codegen-units = 1     # 单编译单元，更好的优化
strip = true          # 去除符号信息
panic = "abort"       # 减少 unwind 代码
```

### 交叉编译方案

- **Windows**: 在 macOS 上用 `cargo-zigbuild` 或 GitHub Actions 的 windows runner 交叉编译
- **macOS**: 本地编译即可（需要 Xcode 命令行工具用于 ScreenCaptureKit）
- 目标三联：`x86_64-pc-windows-msvc`、`aarch64-apple-darwin`、`x86_64-apple-darwin`

## 依赖

| crate     | 用途                          | 条件编译              |
|-----------|-------------------------------|-----------------------|
| cpal      | 麦克风录制                    | 无                    |
| hound     | WAV 写入                      | 无                    |
| lexopt    | CLI 参数解析                  | 无                    |
| windows   | WASAPI loopback (扬声器)      | target_os = "windows" |
| objc2     | ScreenCaptureKit 调用 (扬声器) | target_os = "macos"   |
| block2    | Objective-C block 支持         | target_os = "macos"   |

## 录制进度提示

- 开始录制时打印: "正在录制... (源: 麦克风, 采样率: 16000Hz, 时长: 120s)"
- 每秒打印当前已录制时长
- 结束时打印: "录制完成: output.wav"
