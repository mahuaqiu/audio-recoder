# 离线 NTP 会议音频采播时延 v2 设计

## 1. 文档目的

本文档在审查记录 `docs/2026-07-19-offline-ntp-audio-latency-review.md` 与原实施计划 `docs/2026-07-18-offline-ntp-audio-latency-implementation-plan.md` 的基础上，给出 **schema_version=2** 的修订设计。

目标：在无公网、无额外硬件条件下，用两台 Windows 电脑 + 会议软件扬声器采播链路，得到可验收的会议音频时延测量能力。

范围：

- 仓库 `D:\code\audio-recoder`：chrony 脚本、Windows 时间脚本、recorder 精确时间轴与 sidecar 产出
- 仓库 `D:\code\audio-checker`：v2 sidecar 校验、事件时间插值、时延与误差预算

不在范围：亚毫秒/PTP/GPS、非 Windows speaker loopback、跨午夜录音、会议软件内部改造、由开发环境代跑真实双机。

## 2. 背景与问题

### 2.1 测量定义

```text
会议采播时延 = 接收端检测到测试事件的统一时间
             - 发送端检测到测试事件的统一时间
```

链路：

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

### 2.2 审查确认的阻塞问题

| 编号 | 问题 | 设计对策 |
|---|---|---|
| P1-1 | QPC→UTC 仅在写文件时采一次 | 启动/周期/结束多点校准，packet 用校准序列映射 |
| P1-2 | sidecar 未绑定 WAV，未对照 FSK | `wav_file` + `wav_sha256` + FSK 解码对照 |
| P1-3 | 同步报告无 schema/时效/关键字段硬校验 | 报告升 v2，recorder/checker 强制字段与 10 分钟有效期 |
| P1-4 | 录制后 verify 会重新校时 | 只读 `post_verify` 与 `pre_sync` 彻底拆分 |
| P1-5 | Init 在 WASAPI Start 前就 Success | Start 成功后再通知 |
| P2-1 | `--device` 不真正选设备 | 用匹配到的 `IMMDevice` 初始化 |
| P2-2 | anchor 元数据校验不足 | device_position/QPC/首 anchor/斜率校验 |
| P2-3 | 无跨午夜拒绝 | 两端 `first_pcm` 须同一 UTC 日 |
| P2-4 | 失败路径测试不足 | 固定自动化矩阵 |

### 2.3 协议策略

- 统一升到 **schema_version=2**
- **不兼容** v1 与“半成品宽松校验”
- 旧 v1 解析与降级路径删除

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
     │ pre_sync 报告   │   │ pre_sync 报告   │
     │ WASAPI + QPC    │   │ WASAPI + QPC    │
     │ WAV + v2 sidecar│   │ WAV + v2 sidecar│
     └────────┬────────┘   └────────┬────────┘
              │                     │
              │   录制后 post_verify │
              └──────────┬──────────┘
                         ▼
                 audio-checker (v2 only)
                 绑定校验 + 插值 + 时延
```

两层时间：

1. **chrony/NTP**：两端墙钟对齐到同一离线基准（差值成立，不要求等于真实北京时间）
2. **WASAPI/QPC**：墙钟绑定到 PCM 样点；NTP 不能代替设备时间戳

## 4. v2 协议契约

### 4.1 同步报告

由 `sync-windows-time.ps1`（`report_kind=pre_sync`）或 `verify-windows-time.ps1`（`report_kind=post_verify`）写出：

```json
{
  "schema_version": 2,
  "report_kind": "pre_sync",
  "status": "pass",
  "computer_name": "SENDER-PC",
  "ntp_server": "192.168.10.20",
  "checked_at_unix_ns": 1784351999000000000,
  "sample_count": 10,
  "valid_sample_count": 10,
  "median_offset_ms": 1.4,
  "max_abs_offset_ms": 2.8,
  "rtt_p50_ms": 0.8,
  "threshold_ms": 5.0,
  "windows_time_source": "192.168.10.20,0x8"
}
```

硬约束：

- 必填：`schema_version=2`、`report_kind`、`status`、`ntp_server`（非空）、`checked_at_unix_ns`、`max_abs_offset_ms`（有限数）
- 缺关键字段 → 解析失败
- recorder 仅接受 `pre_sync` + `status=pass`
- 默认有效期：`checked_at` 距录制开始 ≤ 600 秒
- 默认偏差阈值：`max_abs_offset_ms ≤ 5`

### 4.2 WAV sidecar

路径：`<wav完整路径>.timing.json`

```json
{
  "schema_version": 2,
  "clock_domain": "windows-utc-synchronized-by-ntp",
  "source": "wasapi-loopback",
  "wav_file": "sender.wav",
  "wav_sha256": "…",
  "sample_rate": 16000,
  "actual_device_sample_rate": 48000,
  "device_id": "可选",
  "device_name": "可选",
  "first_pcm_utc_unix_ns": 1784352000123456700,
  "first_pcm_millis_of_day": 36000123,
  "fsk_semantics": "first_pcm_sample",
  "fsk_prefix_samples": 14400,
  "recording_started_unix_ns": 1784352000000000000,
  "recording_ended_unix_ns": 1784352120000000000,
  "qpc_utc_calibrations": [
    {
      "phase": "start",
      "qpc_100ns": 987654321000,
      "utc_unix_ns": 1784352000123456700,
      "span_qpc_100ns": 12
    }
  ],
  "clock_jump_detected": false,
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
    "schema_version": 2,
    "report_kind": "pre_sync",
    "server": "192.168.10.20",
    "checked_at_unix_ns": 1784351999000000000,
    "status": "pass",
    "max_abs_offset_ms": 2.8,
    "median_offset_ms": 1.4,
    "rtt_p50_ms": 0.8
  }
}
```

约定：

- `wav_sample_index=0` = FSK 之后第一帧真实 PCM，不是文件物理样点 0
- 纳秒时间字段必须为整数
- `anchors` ≥ 2；`wav_sample_index` / `device_position` / `qpc_100ns` / `utc_unix_ns` 均严格单调
- 首 anchor 的 `wav_sample_index` 必须为 0
- `qpc_utc_calibrations` 至少含 `start` 与 `end`；录制 ≥2s 时应含 `periodic`
- `clock_jump_detected=true` 或 `discontinuities` 非空 → checker 拒绝正式结果

### 4.3 FSK 语义

- FSK 数值 = 第一帧真实 PCM 的统一墙钟时间，精确到毫秒（当天毫秒数）
- 精确到 100ns/ns 的时间与 QPC/设备位置只在 sidecar
- 启用 `--timestamp-mark` 时必须同时生成 v2 sidecar，二者不可拆分
- checker 必须读有效 sidecar；无“仅 FSK”降级路径

### 4.4 删除项

- sidecar / 同步报告 `schema_version=1`
- 无 sidecar 仅靠 FSK 的分析
- 缺失 `max_abs_offset_ms` / `checked_at` 仍继续 success 的宽松逻辑
- `verify-windows-time.ps1` 转调校时脚本的实现

## 5. audio-recorder 设计

### 5.1 启动门槛

启用 `--timestamp-mark` 时必须满足：

| 条件 | 行为 |
|---|---|
| 无 `--time-sync-report` | 拒绝 |
| 报告非 v2 / 缺关键字段 | 拒绝 |
| `report_kind != pre_sync` 或 `status != pass` | 拒绝 |
| 报告超过有效期（默认 600s） | 拒绝 |
| 偏差超过阈值（默认 5ms） | 拒绝 |
| 非 Windows 或非 speaker | 不支持精确 timing |

CLI：

```text
--timestamp-mark
--time-sync-report <PATH>
--require-time-sync
--max-clock-offset <MS>       # 默认 5
--max-sync-report-age <SEC>   # 默认 600
--device <NAME>               # 真正选择 loopback 设备
```

### 5.2 WASAPI 初始化与设备

**Init 成功时机：**

```text
解析目标 IMMDevice
  → Activate IAudioClient
  → GetMixFormat
  → Initialize
  → GetService(IAudioCaptureClient)
  → Start
  → 仅此时发送 InitResult::Success
失败 → InitResult::Failed + 非 0 退出；不得报告“已启动”
```

**设备选择：**

- 无 `--device`：默认 `eRender/eConsole`
- 有 `--device`：枚举 render 设备，按友好名/ID 子串匹配唯一 `IMMDevice`
- 0/多匹配 → 错误并打印可用列表
- 日志打印实际设备；建议写入 sidecar `device_id` / `device_name`

### 5.3 QPC ↔ UTC 映射

独立校准逻辑（建议模块 `src/timing/qpc_utc.rs`）：

```text
每次校准：
  采 8 组 (QPC_before, GetSystemTimePreciseAsFileTime, QPC_after)
  取 span 最短一组
  midpoint_qpc 对应 utc_unix_ns
  记录 { phase, qpc_100ns, utc_unix_ns, span_qpc_100ns }
```

节奏：

| 阶段 | 策略 |
|---|---|
| start | 启动后、写 FSK 前 |
| periodic | 约每 1s |
| end | 停止采集后、写 sidecar 前 |

packet 的 `utc_unix_ns` 由**最近两个校准点**对 QPC 线性插值得到，禁止在结束时用单次映射回填全部历史。

墙钟跳变检测建议阈值：

- 相邻校准点相对 QPC 的斜率偏离 1.0 超过 50ppm 且连续出现，或
- 单次映射跳变 > 5ms  

触发后：`clock_jump_detected=true`，本次不得作为正式结果（checker 必拒）。

### 5.4 捕获与 FSK 时机

```text
Start WASAPI
  → start 校准
  → 等待第一包（得 QPC/设备位置）
  → 映射 first_pcm 时间并生成 FSK
  → 写 FSK + 保护静音 + 缓存的第一包及后续
  → 周期校准与 anchors
  → 停止 → end 校准 → 原子写出
```

- `GetBuffer` 保留 `device_position`、`qpc_position`、`flags`
- 静音包保留时间戳
- 禁止用 `Instant` 补大段静音掩盖 discontinuity；补齐必须记入 `discontinuities`
- 重采样后 anchors 落在目标 WAV 真实 PCM 坐标系

### 5.5 原子写出与绑定

```text
写 output.wav.partial
计算 sha256
写 output.wav.timing.json.partial（含 wav_file、wav_sha256、calibrations、anchors、time_sync 拷贝）
任一步失败：删除 partial，不覆盖旧成功产物，退出非 0
成功：删除旧 sidecar 残留 → 固定顺序 rename 为最终 wav 与 timing.json
sidecar 失败时不得保留“看似成功”的 WAV
```

### 5.6 删除的旧实现

- 结束时一次性 `qpc_to_utc_ns` 回填全部 anchors
- Init 在 Initialize 前 Success
- `--device` 仅名称预检仍录默认设备
- timestamp-mark 无有效同步报告仍继续的路径

## 6. 时间同步脚本与 chrony

### 6.1 文件

```text
scripts/time-sync/
├── install-chrony-server.sh
├── verify-chrony-server.sh
├── sync-windows-time.ps1      # 可改配置，写 pre_sync
├── verify-windows-time.ps1    # 只读，写 post_verify
└── samples/                   # 中/英文 w32tm 解析夹具
```

### 6.2 chrony（宿主机）

- 安装在 Linux 宿主机，不进容器
- 未指定 `--allow-cidr` 时默认 allow 所有 IPv4/IPv6（内网）；公网风险须显式收紧
- 配置幂等 managed block + 时间戳备份
- `local stratum 8`、`makestep 1.0 3`、`rtcsync`
- 仅在运行中的 firewalld/ufw 上放行 UDP/123
- verify 脚本只读：服务 active、UDP/123、`chronyc tracking`
- 回滚：恢复备份、删脚本防火墙规则；默认不卸载 chrony

### 6.3 Windows 校时 vs 只读复检

| 脚本 | 允许 | 禁止 | report_kind |
|---|---|---|---|
| `sync-windows-time.ps1` | `/config`、重启 W32Time、`/resync`、stripchart | — | `pre_sync` |
| `verify-windows-time.ps1` | 连通性、`/query`、stripchart | 调用 sync、`/config`、Restart-Service、`/resync` | `post_verify` |

解析失败必须 fail，不得默认 pass；中/英文 stripchart 均需夹具覆盖。

操作语义：

| pre_sync | post_verify | 本轮 |
|---|---|---|
| pass | pass | 可进正式分析 |
| pass | fail | 时间基准不稳定，作废 |
| fail | * | 禁止开录 |

## 7. audio-checker 设计

### 7.1 输入

```text
--sender / --receiver
--sender-timing / --receiver-timing   # 可选，默认 <wav>.timing.json
--max-clock-error <MS>                # 默认 10
```

无关闭精确校验的开关。只接受 v2。

### 7.2 单端硬校验

1. 协议字段：`schema_version=2`、`fsk_semantics=first_pcm_sample`、`source=wasapi-loopback`、`clock_domain` 匹配  
2. `wav_file` basename 与输入一致  
3. `wav_sha256` 与内容一致  
4. 解码 FSK 与 `first_pcm_millis_of_day` 差 ≤ 1 ms  
5. `time_sync` 完整：v2、`pre_sync`、`pass`、server、`checked_at`、`max_abs_offset_ms`  
6. `checked_at` 与 `recording_started_unix_ns` 间隔 ≤ 600s  
7. `clock_jump_detected=false` 且 `discontinuities` 为空  
8. anchors ≥ 2；首 `wav_sample_index=0`；四字段严格单调  
9. anchors 均在真实 PCM 区间  
10. 相邻 anchor 隐含采样率相对标称 \|rate/nominal−1\| ≤ 1%  
11. calibrations 至少含 start、end  
12. WAV 采样率与 sidecar 一致，`fsk_prefix_samples` 合法  

### 7.3 成对硬校验

- 两端 NTP `server` 相同  
- 相对误差上界 = `|sender.max_abs| + |receiver.max_abs|` ≤ `--max-clock-error`（默认 10）  
- 两端 `first_pcm_utc_unix_ns` 同一 UTC 日  
- 缺 `max_abs_offset_ms` **不得** 以 `None` 上界继续 success  

### 7.4 事件时间

```text
事件索引相对 FSK 后真实 PCM 起点
slope = (right.utc_ns - left.utc_ns) / (right.wav_sample - left.wav_sample)
event_utc_ns = left.utc_ns + (event_sample - left.wav_sample) * slope
latency_ms = (receiver_event_utc_ns - sender_event_utc_ns) / 1e6
```

禁止再加 FSK 前缀 900 ms。anchors < 2 已在门禁拒绝。

### 7.5 成功输出要点

```json
{
  "status": "success",
  "timing_mode": "sidecar-anchors-v2",
  "clock_quality": {
    "sender_server": "...",
    "receiver_server": "...",
    "sender_offset_ms": 1.4,
    "receiver_offset_ms": 0.8,
    "relative_clock_error_bound_ms": 2.2,
    "sender_checked_at_unix_ns": 0,
    "receiver_checked_at_unix_ns": 0
  }
}
```

失败使用稳定 `error.code`（至少 `UNSUPPORTED_TIMING_PROTOCOL`，message 说明原因）。

## 8. 测试矩阵

### 8.1 recorder / 脚本

- 过期报告、缺字段、非 pass、非 pre_sync → 拒绝启动  
- Init 失败：前台/后台非 0，无成功产物  
- 模拟墙钟跳变 → `clock_jump_detected`  
- sidecar 写入失败 → 不留成功 WAV  
- 原子写出后 sha256 与 sidecar 一致  
- `verify-windows-time.ps1` 源码级不含 `/config`、Restart-Service、`/resync`、不调用 sync  
- 中/英文 stripchart 解析夹具  

### 8.2 checker

| 用例 | 预期 |
|---|---|
| 完整 v2 + 已知合成延迟 | success，误差 < 1 hop |
| 缺 sidecar / schema v1 | 拒绝 |
| basename 或 sha256 不符 | 拒绝 |
| FSK 差 > 1 ms | 拒绝 |
| 过期 checked_at | 拒绝 |
| 缺 max_abs_offset_ms | 拒绝 |
| NTP server 不同 | 拒绝 |
| 相对误差超限 | 拒绝 |
| 跨 UTC 日 | 拒绝 |
| device_position/QPC 非单调 | 拒绝 |
| anchor 在 FSK 前缀内 | 拒绝 |
| clock_jump / discontinuities | 拒绝 |
| 48k→16k 时间轴 | 插值正确 |

## 9. 现场验收

### 9.1 环境假设

- 双机 Windows 就绪  
- NTP 宿主机按脚本部署（实施方交付脚本，现场执行安装）  
- 开发侧完成代码/脚本/自动化自检；现场双机与 10 轮测量由用户执行  

### 9.2 一次性

1. 宿主机 `install-chrony-server.sh`  
2. `verify-chrony-server.sh`  
3. 部署 recorder、checker、脚本  
4. 固定会议软件音频配置并记录  

### 9.3 每轮（≥10 轮有效）

```text
verify chrony
  → 两端 pre_sync（≤5ms）
  → 两端提前启动 recorder（speaker + timestamp-mark + 报告）
  → 播放固定测试音（≥5 事件，间隔 3–5s）
  → 停止录音
  → 两端 post_verify（只读，≤5ms）
  → checker（timing_mode=sidecar-anchors-v2, success）
  → 归档 WAV、timing.json、pre/post 报告、checker JSON
```

作废条件：pre/post 失败、进程非成功、discontinuity/clock_jump、相对误差上界 >10ms、事件不对齐或相关过低。

### 9.4 通过标准

| 指标 | 目标 |
|---|---:|
| 每台相对 NTP | ≤ 5 ms（pre 与 post） |
| 相对误差上界 | ≤ 10 ms |
| 样点锚定 | anchors 可用，约 ≤1 ms 分辨率 |
| 同机回环（可选） | P95−P50 ≤ 5 ms |
| 双机固定链路 | ≥10 轮有效；波动建议 ≤10 ms；记录 P50/P95/min/max |

### 9.5 完成定义

1. 两仓库实现本文 v2 契约，旧路径删除  
2. 自动化测试覆盖关键失败路径并通过  
3. 校时与只读复检分离  
4. 现场操作清单与验收模板可用  
5. 用户完成 ≥10 轮有效双机测量且指标达标  
6. 状态文档在现场通过前保持“待现场验收”，通过后更新  

## 10. 实施顺序

1. 契约字段与错误语义对齐（两仓库）  
2. 脚本：verify 只读化、报告 v2、chrony 复核  
3. recorder：Init、设备、周期校准、原子写出、报告校验  
4. checker：v2 门禁、绑定、FSK、跨午夜、强制误差  
5. 自动化测试  
6. 现场清单与 status 文档  
7. 用户双机验收 → 按反馈修阻塞项  

推荐实施顺序约束：先稳定时间基准脚本，再改采集锚定，再改分析，最后现场；避免在 NTP 未稳时调试音频算法。

## 11. 风险

| 风险 | 处理 |
|---|---|
| W32Time 波动 | 有线、同网、同 NTP、测中禁止改时/休眠；前后复检 |
| 宿主机共同漂移 | 不影响差值；禁止测中改宿主机时间 |
| WASAPI 单位/语义误解 | 按 API 与实机诊断日志验证；斜率检查 |
| 重采样破坏时间轴 | 设备位置拼接 + 目标坐标系 anchors |
| 会议软件改波形 | 固定配置、多事件、低相关作废 |
| 选错扬声器 | 真正选 IMMDevice + 打印/记录设备名 |

## 12. 文档关系

| 文档 | 角色 |
|---|---|
| `docs/2026-07-18-offline-ntp-audio-latency-implementation-plan.md` | 原计划（背景仍有效；实现以本文 v2 为准） |
| `docs/2026-07-19-offline-ntp-audio-latency-review.md` | 审查问题清单 |
| **本文** | 修订后的权威设计 |
| 后续 implementation plan | 按本文拆任务落地（writing-plans） |
| implementation status | 跟踪代码与现场完成度 |

---

设计确认日期：2026-07-19  
协议版本：sidecar / 同步报告 schema_version=2  
架构方案：以 v2 时间协议为中心重做 timing 链路（不兼容旧协议）
