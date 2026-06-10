# Audio Recorder - AI 协作指南

## 项目概述

Rust 开发的命令行音频录制工具，支持麦克风和系统音频录制，输出 WAV 格式。

## 构建

```bash
# 开发构建
cargo build

# 发布构建
cargo build --release
```

二进制位置：`target/release/audio-recorder`（约 356KB）

## 运行

```bash
# 录制麦克风（默认）
./audio-recorder -o output.wav

# 录制系统音频（扬声器）
./audio-recorder --source speaker -o output.wav

# 指定参数
./audio-recorder --source microphone --sample-rate 48000 --sample-fmt f32 --duration 60 -o output.wav
```

### 参数说明

| 参数 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--source` | `-s` | microphone | 音频源: microphone, speaker |
| `--sample-rate` | `-r` | 16000 | 采样率 (Hz) |
| `--sample-fmt` | `-f` | s16 | 采样格式: s16, s32, f32 |
| `--duration` | `-d` | 120 | 录制时长 (秒) |
| `--output` | `-o` | recording.wav | 输出文件路径 |
| `--help` | `-h` | | 显示帮助 |

## 平台支持

| 平台 | 麦克风 | 扬声器 |
|------|--------|--------|
| Windows | ✅ cpal | ✅ WASAPI loopback |
| macOS 13.0+ | ✅ cpal | ✅ ScreenCaptureKit |
| macOS 12.x | ✅ cpal | ❌ 需要 macOS 13.0+ |

## 目录结构

```
audio-recoder/
├── src/
│   ├── main.rs           # CLI 入口和 WAV 写入
│   └── capture/
│       ├── mod.rs        # 共享类型和 StopHandle
│       ├── microphone.rs # 麦克风录制
│       ├── wasapi_loopback.rs  # Windows 扬声器
│       └── macos_speaker.rs    # macOS 扬声器
├── Cargo.toml
└── AGENTS.md
```

## 依赖

- `cpal` - 跨平台音频输入
- `hound` - WAV 文件写入
- `lexopt` - CLI 参数解析
- `ctrlc` - Ctrl+C 处理
- `windows` - Windows WASAPI（仅 Windows）
- `screencapturekit` - macOS 屏幕捕获（仅 macOS 13.0+）
- `tokio` - 异步运行时（仅 macOS）

## 开发注意事项

1. **StopHandle** - 统一管理麦克风和扬声器录制，drop 时自动停止
2. **采样格式转换** - 内部统一转为 f64 再写入 WAV
3. **线程安全** - 音频数据通过 mpsc channel 传递
4. **错误处理** - 使用 Result<String, String> 传播错误

## 打包

### Windows

使用 `cargo build --release`，产物为单个可执行文件。

### macOS

同上，需要注意：
- macOS 13.0+ 才能编译 screencapturekit 相关代码
- Swift 版本需 5.9+（Xcode 15+）
