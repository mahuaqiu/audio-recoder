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
# 后台录制麦克风（默认）
./audio-recorder -o output.wav

# 前台阻塞模式录制
./audio-recorder -b -o output.wav

# 录制系统音频（扬声器）
./audio-recorder -s speaker -o output.wav

# 指定参数
./audio-recorder -s microphone -r 48000 -f f32 -d 60 -o output.wav
```

### 参数说明

| 参数 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--source` | `-s` | microphone | 音频源: microphone, speaker |
| `--sample-rate` | `-r` | 16000 | 采样率 (Hz) |
| `--sample-fmt` | `-f` | s16 | 采样格式: s16, s32, f32 |
| `--duration` | `-d` | 120 | 录制时长 (秒) |
| `--output` | `-o` | recording.wav | 输出文件路径 |
| `--device` | `-i` | (系统默认) | 输入设备名称 (模糊匹配) |
| `--list-devices` | `-l` | | 列出所有可用音频设备（麦克风+扬声器） |
| `--blocking` | `-b` | 后台模式 | 前台阻塞模式，等待录制完成 |
| `--help` | `-h` | | 显示帮助 |

### 参数详细说明

#### -s, --source
- `microphone` / `mic`: 麦克风录制
- `speaker` / `spk`: 系统音频录制（扬声器）

#### -r, --sample-rate
- 常用值: 8000, 11025, 16000, 22050, 32000, 44100, 48000, 96000
- 如果设备不支持请求的采样率，会自动适配到最接近的可用值

#### -o, --output
- 若输出路径的父目录不存在，会自动递归创建
- 若 WAV 创建失败（如权限不足），将打印错误并以退出码 1 退出，不再误报"录制完成"

#### -f, --sample-fmt
- `s16`: 16 位整数
- `s32`: 32 位整数  
- `f32`: 32 位浮点
- 如果设备不支持请求的格式，会自动适配

#### -i, --device
- 指定输入设备名称（模糊匹配，不区分大小写）
- 不指定则使用系统默认设备
- 例如：`-i MacBook` 会匹配 "MacBook Pro 麦克风"
- 使用 `--list-devices` 或 `-l` 可列出所有可用设备

#### -l, --list-devices
- 列出所有可用的音频设备名称（包括麦克风输入设备和扬声器输出设备）
- 设备按索引顺序排列，方便确定要使用的设备名
- 无需录制，直接查看可用设备
#### -b, --blocking
- 不加此参数: 后台模式，进程立即返回
- 加此参数: 前台阻塞模式，等待录制完成

## 后台模式使用

### 启动后台录制

```bash
# 后台录制麦克风
./audio-recorder -o myRecording.wav
# 输出类似:
# 正在录制... (源: 麦克风, 采样率: 16000Hz, 格式: s16, 时长: 120s)
# 麦克风设备: 麦克风阵列（适用于数字麦克灵的英特尔@ 智音技术）
# 后台录制已启动，PID: 12345
# 输出文件: myRecording.wav
# PID 文件: .myRecording.pid
# 停止文件: .myRecording.stop (删除此文件可停止录制)
```

### 停止后台录制

有三种方式停止后台录制：

| 方式 | 命令 | 说明 |
|------|------|------|
| 删除停止文件 | `rm .myRecording.stop` | 推荐，优雅停止 |
| 发送中断信号 | `kill -INT 12345` | 等同于 Ctrl+C |
| 杀进程 | `kill 12345` | 不推荐，可能损坏 WAV |

### 查看运行状态

```bash
# 查看正在运行的录制
ls -la .*.stop

# 查看 PID 文件
cat .myRecording.pid
```

### 自动适配

当请求的采样率或格式设备不支持时，会自动适配：

```
提示: 设备不支持请求的参数，已自动适配 - 采样率: 48000Hz, 格式: f32
```

## 平台支持

| 平台 | 麦克风 | 扬声器 |
|------|--------|--------|
| Windows | ✅ cpal | ✅ WASAPI loopback |
| macOS 13.0+ | ✅ cpal | ✅ ScreenCaptureKit |
| macOS 12.x | ✅ cpal | ❌ 需要 macOS 13.0+ |

### Windows WASAPI 注意事项

- 需要 Windows 7 或更高版本
- 扬声器录制使用 WASAPI loopback，需要设备未被独占
- 错误码 `0x88890008` (AUDCLNT_E_DEVICE_IN_USE) 表示设备被其他应用占用

### macOS 注意事项

- 扬声器录制需要 macOS 13.0+ 和屏幕录制权限
- 首次使用时会请求屏幕录制权限，需在系统偏好设置中授权

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
5. **后台模式** - 默认���台运行，使用停止文件机制实现优雅停止
6. **自动适配** - 当设备不支持请求的参数时，自动选择最接近的配置

## 打包

### Windows

使用 `cargo build --release`，产物为单个可执行文件。

### macOS

同上，需要注意：
- macOS 13.0+ 才能编译 screencapturekit 相关代码
- Swift 版本需 5.9+（Xcode 15+）
