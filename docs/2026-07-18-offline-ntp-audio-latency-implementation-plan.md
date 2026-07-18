# 离线 NTP 音频采播时延测量改造实施计划

## 1. 文档目的

本文档给出无公网、无额外硬件条件下，两台 Windows 电脑通过会议软件进行扬声器音频采播时延测量的完整实施方案。

方案覆盖四部分：

1. 在 `zq-platform` Linux 宿主机安装并配置离线 chrony/NTP 服务。
2. 改造 `audio-recorder`，将 WASAPI loopback 音频样点绑定到统一时间轴。
3. 改造 `audio-checker`，使用精确时间元数据计算发送端和接收端事件时间。
4. 提供每次测试前在两台 Windows 电脑执行的时间同步与健康检查脚本。

本文档是实施计划，不代表当前代码已经具备这些能力。

## 2. 场景与约束

### 2.1 测试链路

```text
发送端固定测试音频播放
        ↓
发送端 WASAPI loopback 录制
        ↓
会议软件采集、编码、传输
        ↓
接收端会议软件解码、播放
        ↓
接收端 WASAPI loopback 录制
```

测量值定义为：

```text
会议采播时延 = 接收端检测到测试事件的统一时间
             - 发送端检测到测试事件的统一时间
```

### 2.2 已确认约束

- 发送端和接收端都录制扬声器音频。
- 两段录音在同一天完成，不处理跨天配对。
- 测试环境没有公网。
- 不引入 GPS、PPS、Word Clock 等额外硬件。
- 可以管理部署 `zq-platform` 的宿主机。
- `zq-platform` 当前运行于 Docker，FastAPI 不是 NTP 服务。
- 目标优先满足会议音频毫秒级时延测量，不承诺亚毫秒级计量能力。

### 2.3 精度目标

第一阶段建议设置如下验收目标：

| 指标 | 目标 |
|---|---:|
| 两台 Windows 电脑相对 NTP 服务器偏差 | 每台绝对值不超过 5 ms |
| 两台电脑相对偏差上界 | 不超过 10 ms |
| WASAPI 样点时间锚定分辨率 | 不高于 1 ms |
| 同机回环重复测量波动 | P95-P50 不超过 5 ms |
| 双机固定链路重复测量波动 | 根据会议软件验收，建议不超过 10 ms |

若最终要求稳定达到 `±1 ms`，仅依靠普通 Windows NTP、软件时间戳和普通网卡很难提供可证明保证，需要重新评估 PTP 或硬件时间基准。

## 3. 总体架构

```text
                    局域网 UDP/123
              ┌─────────────────────┐
              │ zq-platform 宿主机  │
              │ chrony/NTP 服务     │
              └──────────┬──────────┘
                         │
              ┌──────────┴──────────┐
              │                     │
     ┌────────▼────────┐   ┌────────▼────────┐
     │ Windows 发送端  │   │ Windows 接收端  │
     │ W32Time 同步    │   │ W32Time 同步    │
     │ WASAPI + QPC    │   │ WASAPI + QPC    │
     └────────┬────────┘   └────────┬────────┘
              │                     │
              │ WAV + timing.json   │
              └──────────┬──────────┘
                         ▼
                 audio-checker
                 事件对齐与时延计算
```

本方案使用两个互补时间层：

1. **chrony/NTP 层**：让两台 Windows 电脑的系统墙钟对齐到同一个离线服务器。
2. **WASAPI/QPC 层**：把系统墙钟准确绑定到录音中的 PCM 样点位置。

NTP 不能代替音频设备时间戳；WASAPI 的本机 QPC 也不能单独解决跨电脑时钟偏差。

## 4. 时间模型

### 4.1 统一时间不要求是真实北京时间

平台宿主机在离线环境中可以把自己的系统时钟作为统一基准。即使该时钟与真实北京时间存在固定偏差，只要测试期间稳定，且两台录音电脑都同步到它，差值计算仍然成立。

### 4.2 PCM 时间锚点

对每个录音文件至少保存下列锚点：

```text
WAV 中真实音频样点索引
WASAPI QPC 时间
采样该 QPC 附近的 Windows UTC 时间
WASAPI 设备位置
```

事件时间使用分段线性插值计算：

```text
event_time = anchor.utc_time
           + (event_sample - anchor.wav_sample) * slope
```

其中 `slope` 由相邻时间锚点拟合，不直接假定标称 `16000 Hz` 或 `48000 Hz` 永远等于真实采样速率。

### 4.3 FSK 的定位

继续使用 27 位“当天毫秒数”的 FSK 位结构，但直接切换为新时间语义，不兼容历史录音协议。FSK 用于单文件识别和诊断，最高精度时间计算使用 timing sidecar。

新版本约定：

- FSK 数值表示第一帧真实 PCM 的统一墙钟时间，精确到毫秒。
- WAV 文件仍由 `FSK 前缀 + 保护静音 + 真实 PCM` 组成。
- 精确到 100 ns/ns 级表示的时间、QPC 和设备位置存入 sidecar JSON。
- 启用 FSK 时间标记时必须同时生成 timing sidecar，二者不可拆分。
- `audio-checker` 必须读取有效 sidecar，不提供仅依靠 FSK 的降级计算。

历史 WAV 中的 FSK 表示程序启动前读取的系统时间，与新协议语义不同。新 checker 不分析此类文件；sidecar 缺失或协议版本不匹配时直接返回 `UNSUPPORTED_TIMING_PROTOCOL`。

## 5. 第一部分：宿主机 chrony/NTP 部署

### 5.1 部署原则

- chrony 安装在 `zq-platform` 的 Linux 宿主机，不安装在 FastAPI 容器中。
- 不修改 `zq-platform` 的 FastAPI、PostgreSQL、Redis 或 Nginx 容器。
- NTP 使用 UDP `123`，仅向指定局域网网段开放。
- 无公网时启用 chrony local reference，使宿主机自身时钟成为局域网基准。
- 生产部署前备份现有 chrony 配置，脚本需要支持重复执行。

### 5.2 建议新增文件

在本仓库新增：

```text
scripts/time-sync/
├── install-chrony-server.sh     # 宿主机一次性安装和配置
├── verify-chrony-server.sh      # 宿主机服务检查
├── sync-windows-time.ps1        # Windows 每次测试前同步
└── verify-windows-time.ps1      # Windows 偏差采样与验收
```

宿主机脚本已在本仓库创建，但不会由本地开发环境执行。部署人员需要将脚本复制到远端宿主机后人工执行。

### 5.3 安装脚本参数

`install-chrony-server.sh` 建议接口：

```bash
sudo bash install-chrony-server.sh \
  --allow-cidr 192.168.10.0/24 \
  --bind-address 192.168.10.20 \
  --local-stratum 8
```

参数说明：

| 参数 | 必填 | 说明 |
|---|---|---|
| `--allow-cidr` | 是 | 允许访问 NTP 的测试网段，可重复指定 |
| `--bind-address` | 否 | NTP 监听地址；不指定时由 chrony 默认监听 |
| `--local-stratum` | 否 | 离线本地时钟层级，默认 8 |
| `--dry-run` | 否 | 只展示将执行的操作 |

脚本必须：

1. 检查当前用户是否具有 root 权限。
2. 识别 Debian/Ubuntu 的 `apt` 或 RHEL 系的 `dnf/yum`。
3. 安装 `chrony`。
4. 检测现有 `/etc/chrony/chrony.conf` 或 `/etc/chrony.conf`。
5. 以带时间戳文件名备份原配置。
6. 使用独立受管配置块写入配置，重复执行时替换该块而不是重复追加。
7. 配置 `local stratum 8`，允许离线提供时间。
8. 配置指定的 `allow <CIDR>`。
9. 配置 `makestep 1.0 3` 和 `rtcsync`。
10. 校验配置语法后再重启服务。
11. 启用 `chronyd` 开机启动。
12. 检查 UDP 123 是否监听。
13. 打印服务器 IP 和 Windows 客户端下一步命令。

建议配置片段：

```conf
# BEGIN audio-latency managed block
local stratum 8
allow 192.168.10.0/24
makestep 1.0 3
rtcsync
# END audio-latency managed block
```

是否使用 `orphan` 选项需根据宿主机 chrony 版本验证，第一版不依赖该选项。

### 5.4 防火墙处理

安装脚本只操作检测到且正在运行的防火墙，不能无条件修改系统安全策略。

- `firewalld`：添加来源网段到 UDP 123 的永久规则。
- `ufw`：添加来源网段到 UDP 123 的允许规则。
- 未检测到受支持防火墙时打印人工配置提示。

所有规则必须限制来源网段，禁止直接开放给所有网络。

### 5.5 宿主机验证脚本

`verify-chrony-server.sh` 应检查：

```bash
systemctl is-active chronyd || systemctl is-active chrony
chronyc tracking
chronyc sources -v
chronyc clients
ss -lunp
```

成功标准：

- chrony 服务状态为 `active`。
- UDP 123 正在预期地址监听。
- `chronyc tracking` 可正常返回。
- 从 Windows 执行 `w32tm /stripchart` 可收到响应。

### 5.6 回滚方案

安装脚本执行前必须输出备份文件路径。回滚步骤：

1. 停止 chrony 服务。
2. 恢复备份配置。
3. 删除脚本创建的防火墙规则。
4. 重启原时间服务。
5. 再次检查 UDP 123 和系统时间状态。

默认不卸载 chrony，避免删除发行版原有依赖；只有明确指定 `--uninstall` 才考虑卸载。

## 6. 第二部分：audio-recorder 改造

### 6.1 当前问题

当前实现存在三项关键误差：

1. 在启动 WASAPI 之前读取 `Local::now()`，不是第一帧 PCM 的时间。
2. `IAudioCaptureClient::GetBuffer` 丢弃设备位置和 QPC 时间戳。
3. 使用 `Instant::elapsed()` 推测并补齐静音，不能作为精确设备时间轴。

### 6.2 改造目标

- 仅对 Windows 扬声器 WASAPI loopback 建立精确时间轴。
- 第一个真实音频包到达后再确定 FSK 时间。
- 保存录制期间的周期性时间锚点和 discontinuity 信息。
- 保持不带 `--timestamp-mark` 时的原有行为。
- 不兼容旧 FSK 时间语义，录制器与 checker 必须同步升级。

### 6.3 建议的数据结构

在 capture 层将 channel 数据从裸 `Vec<f64>` 改为带元数据的数据包：

```rust
pub struct CapturedPacket {
    pub samples: Vec<f64>,
    pub device_position: Option<u64>,
    pub qpc_100ns: Option<u64>,
    pub flags: u32,
}
```

新增录音时间信息：

```rust
pub struct TimingAnchor {
    pub wav_sample_index: u64,
    pub device_position: u64,
    pub qpc_100ns: u64,
    pub utc_unix_ns: i128,
}
```

精确字段类型需结合 `windows-rs 0.58` 的 API 签名验证，禁止假定 WASAPI 返回值单位而不写自动测试和目标机诊断日志。

### 6.4 WASAPI 捕获改造

修改 `src/capture/wasapi_loopback.rs`：

1. 调用 `GetBuffer` 时接收 `device_position` 和 `qpc_position`。
2. 验证 `qpc_position` 在当前 Windows SDK 中是否为 100 ns 单位。
3. 记录 `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`。
4. 静音包也保留其时间戳，不因数据指针为空而丢失时间轴。
5. 不再用轮询线程的 `Instant` 作为主要 PCM 时间来源。
6. 禁止用大段补静音掩盖设备 discontinuity；确需补齐时必须记录补齐区间。
7. 每个包携带其时间元数据发送到 writer。

第一阶段可以保留当前轮询模型，但时间计算只能使用 WASAPI 返回的时间戳。后续可改为事件驱动 WASAPI，降低 CPU 占用和轮询抖动。

### 6.5 QPC 与 UTC 映射

录制开始时及录制过程中周期性采集一组本机时钟对：

```text
QPC before
GetSystemTimePreciseAsFileTime
QPC after
```

使用 `QPC before` 与 `QPC after` 的中点作为 UTC 采样对应的 QPC，减小调用顺序误差。

每次采集多组，选取 QPC 包围区间最短的一组。建议：

- 启动时采集 8 组，选择最短区间。
- 录制过程中每 1 秒采集一个锚点。
- 结束时再采集 8 组。
- 如果墙钟发生跳变，记录异常并使本次结果失败。

### 6.6 timing sidecar 格式

录制 `sender.wav` 时生成：

```text
sender.wav
sender.wav.timing.json
```

建议格式：

```json
{
  "schema_version": 1,
  "clock_domain": "windows-utc-synchronized-by-ntp",
  "source": "wasapi-loopback",
  "wav_file": "sender.wav",
  "sample_rate": 16000,
  "actual_device_sample_rate": 48000,
  "first_pcm_utc_unix_ns": 1784352000123456700,
  "first_pcm_millis_of_day": 36000123,
  "fsk_semantics": "first_pcm_sample",
  "fsk_prefix_samples": 14400,
  "anchors": [
    {
      "wav_sample_index": 0,
      "device_position": 123456,
      "qpc_100ns": 987654321000,
      "utc_unix_ns": 1784352000123456700
    }
  ],
  "discontinuities": [],
  "time_sync": {
    "server": "192.168.10.20",
    "checked_at_unix_ns": 1784351999000000000,
    "offset_ms": 1.4,
    "rtt_ms": 0.8,
    "status": "pass"
  }
}
```

约定：

- `wav_sample_index=0` 表示 FSK 之后第一帧真实 PCM，不表示整个 WAV 文件物理样点 0。
- `fsk_prefix_samples` 表示文件中 FSK 与保护静音总长度。
- 所有整数时间字段保持整数，禁止用 JSON 浮点数保存纳秒时间。
- sidecar 使用临时文件写完后原子替换，避免进程异常留下看似完整的 JSON。
- WAV 写入失败时不保留成功状态的 sidecar。

### 6.7 FSK 生成时机

新的 writer 流程：

```text
启动 WASAPI
    ↓
等待第一包（获得 QPC/设备位置）
    ↓
计算第一帧 PCM 的统一墙钟时间
    ↓
生成对应当天毫秒 FSK
    ↓
写入 FSK 和保护静音
    ↓
写入已经缓存的第一包及后续真实 PCM
```

第一包在内存中短暂缓存，不能为等待 FSK 而丢弃。

### 6.8 命令行改造

建议新增参数：

```text
--time-sync-report <PATH>  读取测试前同步脚本生成的报告
--require-time-sync        同步状态不合格时拒绝录制
--max-clock-offset <MS>    允许的最大绝对偏差，默认 5
```

推荐正式测试命令：

```powershell
audio-recorder.exe `
  --source speaker `
  --sample-rate 16000 `
  --sample-fmt s16 `
  --duration 120 `
  --output sender.wav `
  --timestamp-mark `
  --time-sync-report .\time-sync-report.json `
  --require-time-sync `
  --blocking
```

### 6.9 协议切换策略

| 输入/参数 | 行为 |
|---|---|
| 无 `--timestamp-mark` | 与当前行为一致 |
| `--timestamp-mark` | 生成新语义 FSK 和 timing sidecar |
| 非 Windows speaker | 第一阶段返回“不支持精确 timing”错误 |
| 历史 WAV 或 sidecar 缺失 | checker 返回 `UNSUPPORTED_TIMING_PROTOCOL` |

本次升级视为时间协议的不兼容切换。录制器输出的 sidecar 必须包含 `schema_version=1` 和 `fsk_semantics=first_pcm_sample`；checker 必须同时校验这两个字段。

## 7. 第三部分：audio-checker 改造

### 7.1 改造目标

- 自动发现发送端和接收端对应的 `.timing.json`。
- 对每个事件按时间锚点计算统一墙钟时间。
- 检查两端同步质量、元数据完整性和音频 discontinuity。
- sidecar 不存在或协议版本不匹配时拒绝计算。
- 在结果中明确输出采用的时间模式和误差预算。

### 7.2 建议新增模块

在 `D:\code\audio-checker` 中规划：

```text
src/
├── timing.rs          # sidecar 读取、校验、锚点插值
├── clock_quality.rs   # 同步质量和误差预算
└── analyze.rs         # 接入新的事件时间计算
```

### 7.3 sidecar 查找规则

默认查找：

```text
<wav完整路径>.timing.json
```

例如：

```text
D:\records\sender.wav
D:\records\sender.wav.timing.json
```

允许显式指定：

```text
--sender-timing <PATH>
--receiver-timing <PATH>
```

禁止仅按文件名匹配其他目录中的 timing 文件，避免拿错测试批次。

### 7.4 新事件时间公式

有 sidecar 时：

1. checker 检测的事件索引相对于 FSK 后真实音频起点。
2. 使用事件左右最近的 timing anchors 插值。
3. 只有一个锚点时可按标称采样率外推，但必须给出漂移未校正警告。

```text
slope = (right.utc_ns - left.utc_ns)
      / (right.wav_sample - left.wav_sample)

event_utc_ns = left.utc_ns
             + (event_sample - left.wav_sample) * slope
```

新协议统一使用：

```text
新事件时间 = first_pcm_time + 事件在真实 PCM 中的校正偏移
```

新公式不得再次加 900 ms FSK 前缀。

### 7.5 元数据校验

任一条件不满足时应失败：

- WAV 采样率与 sidecar 不一致。
- `schema_version` 不支持。
- `source` 不是 `wasapi-loopback`。
- anchors 样点索引或 UTC 时间不单调。
- anchors 超出 WAV 真实音频范围。
- 出现 WASAPI discontinuity 且影响事件附近时间轴。
- `time_sync.status` 不是 `pass`。
- 同步报告距离录制开始超过允许时间，例如 10 分钟。
- 两端 NTP server 不同。
- 两端记录日期不同。

### 7.6 误差预算

结果中增加：

```json
{
  "timing_mode": "sidecar-anchors-v1",
  "clock_quality": {
    "sender_offset_ms": 1.4,
    "receiver_offset_ms": -0.8,
    "relative_clock_error_bound_ms": 2.2,
    "sender_rtt_ms": 0.7,
    "receiver_rtt_ms": 0.9
  },
  "warnings": []
}
```

相对时钟误差保守上界第一版可使用：

```text
abs(sender_offset_ms) + abs(receiver_offset_ms)
```

后续若同步脚本能输出统计置信区间，再改用更合理的置信边界。

### 7.7 命令行行为

推荐命令保持简单：

```powershell
audio-checker.exe `
  --sender D:\records\sender.wav `
  --receiver D:\records\receiver.wav `
  --count 5 `
  --pretty
```

新增参数：

```text
--sender-timing <PATH>       显式发送端 timing 文件
--receiver-timing <PATH>     显式接收端 timing 文件
--max-clock-error <MS>       最大允许相对时钟误差，默认 10
```

有效 sidecar 是 checker 的强制输入条件，不提供关闭精确时间校验的命令行选项。

## 8. 第四部分：每次测试前的 Windows 同步脚本

### 8.1 脚本职责

`sync-windows-time.ps1` 在发送端和接收端分别执行，负责：

1. 检查管理员权限。
2. 检查 NTP 服务器 UDP 123 基本可达性。
3. 配置 Windows Time 使用指定的内部 NTP 服务器。
4. 启动或重启 `W32Time`。
5. 强制重新发现时间源并同步。
6. 通过 `w32tm /stripchart` 连续采样偏差。
7. 解析有效样本，计算中位数、最大绝对偏差和 RTT 可用指标。
8. 根据阈值生成 pass/fail。
9. 输出供 recorder 读取的 JSON 报告。

### 8.2 使用方式

在两台 Windows 电脑上，以管理员 PowerShell 执行：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\sync-windows-time.ps1 `
  -NtpServer 192.168.10.20 `
  -Samples 20 `
  -MaxOffsetMs 5 `
  -OutputPath .\time-sync-report.json
```

脚本内部核心命令：

```powershell
w32tm /config /manualpeerlist:"192.168.10.20,0x8" /syncfromflags:manual /update
Restart-Service W32Time
w32tm /resync /rediscover
w32tm /query /source
w32tm /query /status
w32tm /stripchart /computer:192.168.10.20 /samples:20 /dataonly
```

注意：`w32tm` 输出会受 Windows 显示语言影响。实现时应为中文和英文输出各加入测试样本；解析失败必须返回失败，不能默认为同步成功。

### 8.3 同步报告格式

```json
{
  "schema_version": 1,
  "status": "pass",
  "computer_name": "SENDER-PC",
  "ntp_server": "192.168.10.20",
  "checked_at_unix_ns": 1784351999000000000,
  "sample_count": 20,
  "valid_sample_count": 20,
  "median_offset_ms": 1.4,
  "max_abs_offset_ms": 2.8,
  "threshold_ms": 5.0,
  "windows_time_source": "192.168.10.20,0x8"
}
```

### 8.4 每次测试前标准操作

两台电脑都执行：

```powershell
# 1. 校时并生成报告
.\sync-windows-time.ps1 `
  -NtpServer 192.168.10.20 `
  -Samples 20 `
  -MaxOffsetMs 5 `
  -OutputPath .\time-sync-report.json

# 2. 确认退出码为 0
if ($LASTEXITCODE -ne 0) {
  throw "时间同步未通过，禁止开始录制"
}

# 3. 启动录制
.\audio-recorder.exe `
  -s speaker `
  -r 16000 `
  -f s16 `
  -d 120 `
  -o .\recording.wav `
  -t `
  --time-sync-report .\time-sync-report.json `
  --require-time-sync `
  -b
```

两台机器不需要同时按下回车，也不需要录制开始时间相同。建议都提前 5 至 10 秒开始录制，确认录制已稳定后再播放测试音频。

### 8.5 测试结束后的复检

录制完成后，两台电脑再次执行只检查、不修改配置的脚本：

```powershell
.\verify-windows-time.ps1 `
  -NtpServer 192.168.10.20 `
  -Samples 20 `
  -MaxOffsetMs 5 `
  -OutputPath .\time-sync-post-report.json
```

如果录制前通过、录制后失败，则本次测试标记为时间基准不稳定，不进入正式验收结果。

## 9. 固定测试音频建议

虽然时间同步是本计划重点，固定测试音频仍应满足稳定对齐要求：

- 使用确定性生成的 WAV，不使用流媒体或在线资源。
- 每段测试至少包含 5 个定位事件。
- 相邻事件间隔建议 3 至 5 秒。
- 使用 chirp、PN 序列或重复一致的短脉冲，不依赖最大音量点。
- 发送端和接收端都保留完整前后静音。
- 禁止在测试过程中切换默认扬声器设备。
- 固定会议软件的降噪、自动增益和音乐模式配置，并记录到测试报告。

## 10. 实施阶段

### 阶段 1：chrony 部署脚本

交付物：

- `scripts/time-sync/install-chrony-server.sh`
- `scripts/time-sync/verify-chrony-server.sh`
- 宿主机部署说明和回滚说明

验收：

- 在无公网环境重启宿主机后 chrony 自动运行。
- 两台 Windows 电脑均能从宿主机读取时间。
- 非允许网段无法访问 UDP 123。

### 阶段 2：Windows 同步脚本

交付物：

- `scripts/time-sync/sync-windows-time.ps1`
- `scripts/time-sync/verify-windows-time.ps1`
- 中文/英文 `w32tm` 输出解析测试样本

验收：

- 同步成功返回退出码 0 和 `status=pass` JSON。
- 偏差超限、无管理员权限、服务器不可达、解析失败均返回非 0。
- 报告可被 recorder 校验。

### 阶段 3：recorder 精确时间轴

交付物：

- WASAPI packet 时间元数据。
- QPC 与 UTC 映射。
- FSK 第一 PCM 语义。
- `.timing.json`。
- discontinuity 检测。

验收：

- FSK 解码时间与 `first_pcm_millis_of_day` 一致，允许 1 ms 取整差。
- anchors 单调且覆盖录制时间。
- 录制 10 分钟后拟合采样率合理，不出现异常跳变。
- 模拟 discontinuity 时结果明确失败或警告。

### 阶段 4：checker 精确计算

交付物：

- sidecar 解析与 Schema 校验。
- anchor 插值。
- 旧时间协议拒绝逻辑。
- 同步质量和误差预算输出。

验收：

- 合成样本能够恢复已知延迟，误差小于 1 个分析 hop。
- 新 sidecar 模式不会额外增加 900 ms。
- 历史文件和缺少 sidecar 的文件均返回 `UNSUPPORTED_TIMING_PROTOCOL`。

### 阶段 5：双机现场验收

按顺序执行：

1. 宿主机 chrony 服务检查。
2. 两台 Windows 电脑录制前同步。
3. 两端提前开始 WASAPI loopback 录制。
4. 发送端播放固定测试音频并通过会议软件发送。
5. 两端录制完成后复检时间偏差。
6. 汇总 WAV、sidecar、同步前后报告。
7. checker 使用 precise timing 模式计算。
8. 连续执行至少 10 轮，统计中位数、P95、最小值和最大值。

## 11. 测试矩阵

| 测试 | 预期结果 |
|---|---|
| NTP 正常、sidecar 完整 | 正常输出精确模式结果 |
| 两台电脑录制启动相差 10 秒 | 延迟结果不受启动差影响 |
| Windows 偏差超过 5 ms | 同步脚本失败，recorder 拒绝启动 |
| NTP 服务器不可达 | 同步脚本失败 |
| sidecar 缺失 | checker 返回 `UNSUPPORTED_TIMING_PROTOCOL` |
| sidecar 协议版本不匹配 | checker 返回 `UNSUPPORTED_TIMING_PROTOCOL` |
| anchors 不单调 | checker 返回 timing metadata 错误 |
| 录制中墙钟跳变 | recorder 标记异常，checker 拒绝正式结果 |
| WASAPI discontinuity | 事件受影响时拒绝结果 |
| 录制跨午夜 | 第一版明确拒绝或警告，不进入正式结果 |
| sender/receiver NTP server 不同 | checker 拒绝正式结果 |
| 48 kHz 设备重采样到 16 kHz | 时间轴按设备位置和目标 WAV 样点正确映射 |

## 12. 风险与处理

### 12.1 Windows W32Time 精度波动

处理：测试前后都采样；偏差超限即拒绝；使用有线局域网；两台机器连接同一交换网络和同一 NTP 服务器。

### 12.2 宿主机时钟漂移

处理：双机共同跟随同一宿主机时，短时共同漂移主要影响真实北京时间，不直接影响两端差值；仍需保证宿主机测试期间不被人工改时、不休眠、不切换时间源。

### 12.3 WASAPI 时间戳语义和单位理解错误

处理：依据微软 API 文档和 `windows-rs` 实际签名实现；在目标 Windows 机器输出诊断数据；使用已知周期测试音验证时间斜率。

### 12.4 重采样破坏时间轴

处理：sidecar 同时记录设备采样率和 WAV 采样率；anchors 使用 WAV 真实音频区样点索引；禁止按每个 packet 独立 `ceil` 重采样造成累计长度偏差，应使用带连续相位状态的流式重采样器。

### 12.5 会议软件音频处理改变波形

处理：使用抗编解码的 chirp/PN 定位信号和多事件统计；固定会议软件音频配置；低相关性时拒绝输出。

## 13. 运维与现场检查清单

### 13.1 一次性部署

- [ ] 确认 `zq-platform` 宿主机为 Linux，记录发行版。
- [ ] 确认宿主机固定 IP。
- [ ] 确认测试网段 CIDR。
- [ ] 执行 chrony 安装脚本。
- [ ] 配置防火墙 UDP 123。
- [ ] 重启宿主机后验证 chrony。
- [ ] 在两台 Windows 电脑部署同步脚本和工具。

### 13.2 每次测试前

- [ ] 宿主机 `verify-chrony-server.sh` 通过。
- [ ] 发送端同步脚本通过。
- [ ] 接收端同步脚本通过。
- [ ] 两端报告使用相同 NTP server。
- [ ] 两端最大绝对偏差均不超过 5 ms。
- [ ] 两端选择正确扬声器设备。
- [ ] 会议软件音频配置与基线一致。

### 13.3 每次测试后

- [ ] 两端时间复检通过。
- [ ] 两端 WAV 可正常打开。
- [ ] 两端 sidecar 存在且完整。
- [ ] 两端无影响事件的 discontinuity。
- [ ] checker 成功校验两端 timing sidecar。
- [ ] 保存同步报告、WAV、sidecar 和 checker JSON。

## 14. 完成定义

本方案在满足以下条件时视为完成：

1. 离线 NTP 服务可通过脚本安装、验证和回滚。
2. 两台 Windows 电脑可通过统一脚本完成同步并输出机器可读报告。
3. recorder 的 FSK 时间绑定第一帧真实 PCM，而不是程序启动时间。
4. recorder 为每个 WAV 输出包含周期 anchors 的 timing sidecar。
5. checker 能按 anchors 计算事件统一时间，并输出时钟误差预算。
6. checker 强制使用新时间协议，历史文件和缺少 sidecar 的文件会被明确拒绝。
7. 双机至少 10 轮现场测试中没有时间轴跳变、900 ms 重复补偿或启动时间相关偏差。
8. 操作人员只需要执行文档中的宿主机检查、Windows 同步、录制和 checker 命令即可完成测试。

## 15. 推荐实施顺序

严格按以下顺序实施，避免在时间基准尚未稳定时调试音频算法：

1. 实现并部署宿主机 chrony 脚本。
2. 实现 Windows 同步与验证脚本。
3. 先在两台电脑验证 NTP 偏差和稳定性。
4. 改造 recorder 的 WASAPI 时间戳和 sidecar。
5. 使用单机合成/回环数据验证 PCM 时间轴。
6. 改造 checker 的 sidecar 插值和误差输出。
7. 执行双机会议软件现场验收。
