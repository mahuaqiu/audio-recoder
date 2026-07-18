# 离线 NTP 会议音频采播时延 v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 `audio-recoder` 与 `audio-checker` 升级到 schema_version=2 的精确时间协议，修复审查中的 P1/P2 问题，达到可做正式双机会议采播验收的代码与脚本状态。

**Architecture:** 以 v2 时间契约为中心：Windows 脚本产出 `pre_sync`/`post_verify` 报告；recorder 在录制全过程采集 QPC↔UTC 校准点、用 WASAPI 时间戳建 anchors，原子写出 WAV+sidecar；checker 强制绑定 WAV/FSK/同步报告并插值事件时间。不兼容 v1。

**Tech Stack:** Rust 2021（两个 crate）、Windows WASAPI via `windows` 0.58、PowerShell `w32tm`、Linux bash+chrony、serde_json sidecar。

**Spec:** `docs/superpowers/specs/2026-07-19-offline-ntp-audio-latency-v2-design.md`

## Global Constraints

- sidecar / 同步报告只接受 `schema_version=2`；删除 v1 宽松路径
- 精确 timing 仅 Windows speaker WASAPI loopback + `--timestamp-mark`
- 同步报告有效期默认 600s；单机偏差默认 5ms；相对误差上界默认 10ms
- `verify-windows-time.ps1` 禁止 `/config`、`Restart-Service`、`/resync`、调用 sync 脚本
- InitResult::Success 必须在 `Initialize`+`GetService`+`Start` 全部成功之后
- anchors 的 UTC 必须由录制期校准序列映射，禁止结束时单次映射回填
- 所有对话/注释使用中文；提交信息可用中文
- 两仓库分别提交；协议字段名跨仓库必须一致
- 现场 10 轮双机由用户执行，本计划交付代码+脚本+清单+自动化测试

---

## File Structure

### `audio-recoder`

| 路径 | 职责 |
|---|---|
| `src/timing/mod.rs` | timing 子模块出口 |
| `src/timing/sync_report.rs` | 读取/校验 v2 同步报告 |
| `src/timing/qpc_utc.rs` | QPC↔UTC 多点校准、跳变检测、packet 映射 |
| `src/timing/sidecar.rs` | v2 sidecar 结构体与原子写出 |
| `src/timing/sha256.rs` | WAV 内容 sha256（纯 Rust 实现或 `sha2` 依赖） |
| `src/main.rs` | CLI、录制流程编排、接入 timing 模块 |
| `src/capture/mod.rs` | `RecordConfig` 增加 `max_sync_report_age_secs` 等 |
| `src/capture/wasapi_loopback.rs` | Init 时机、真实设备选择、可选设备元数据 |
| `scripts/time-sync/sync-windows-time.ps1` | v2 `pre_sync` 报告 |
| `scripts/time-sync/verify-windows-time.ps1` | 只读 v2 `post_verify` |
| `scripts/time-sync/samples/*.txt` | 中英文 stripchart 夹具 |
| `scripts/time-sync/tests/Parse-Stripchart.Tests.ps1` | 解析单测（可选；若无 Pester 则用固定文本+小型 ps1 断言） |
| `docs/field-acceptance-checklist-v2.md` | 现场操作与验收清单 |

### `audio-checker`

| 路径 | 职责 |
|---|---|
| `src/timing.rs` | v2 结构体、单端硬校验、插值 |
| `src/analyze.rs` | 成对校验、FSK 对照、误差预算、`timing_mode` |
| `tests/integration_tests.rs` | v2 成功路径 + 失败路径矩阵 |
| `src/timestamp.rs` | 已有 FSK 解码，供对照使用（一般不改协议） |

---

### Task 1: 同步报告 v2 与只读 verify 拆分（audio-recoder）

**Files:**
- Modify: `scripts/time-sync/sync-windows-time.ps1`
- Modify: `scripts/time-sync/verify-windows-time.ps1`
- Create: `scripts/time-sync/samples/stripchart-en.txt`
- Create: `scripts/time-sync/samples/stripchart-zh.txt`
- Create: `scripts/time-sync/Parse-Stripchart.ps1`（共享解析函数，供两脚本点源）
- Create: `scripts/time-sync/assert-verify-readonly.ps1`（静态检查 verify 不含危险命令）

**Interfaces:**
- Produces: v2 JSON 字段  
  `schema_version=2`, `report_kind` (`pre_sync`|`post_verify`), `status`, `computer_name`, `ntp_server`, `checked_at_unix_ns`, `sample_count`, `valid_sample_count`, `median_offset_ms`, `max_abs_offset_ms`, `rtt_p50_ms`（可先写 `$null` 若 stripchart 无 RTT）, `threshold_ms`, `windows_time_source`

- [ ] **Step 1: 写 stripchart 夹具**

`scripts/time-sync/samples/stripchart-en.txt`:

```text
Tracking 192.168.10.20 [192.168.10.20:123].
Collecting 5 samples.
The current time is 7/19/2026 10:00:00 AM.
10:00:01, d:+00.0012345s
10:00:02, d:-00.0008000s
10:00:03, d:+00.0011000s
10:00:04, d:+00.0009000s
10:00:05, d:+00.0010000s
```

`scripts/time-sync/samples/stripchart-zh.txt`（按实机中文输出调整；至少覆盖 `±0.00x s` 模式）:

```text
正在跟踪 192.168.10.20 [192.168.10.20:123]。
正在收集 5 个样本。
10:00:01, d:+00.0012345s
10:00:02, d:-00.0008000s
10:00:03, d:+00.0011000s
10:00:04, d:+00.0009000s
10:00:05, d:+00.0010000s
```

- [ ] **Step 2: 抽取共享解析函数 `Parse-Stripchart.ps1`**

```powershell
function Get-OffsetMillisecondsFromStripchart {
    param([string[]]$Lines)
    $values = @(
        $Lines | ForEach-Object {
            $match = [regex]::Match([string]$_, '([+-]?\d+(?:\.\d+)?)s')
            if ($match.Success) { [double]$match.Groups[1].Value * 1000.0 }
        } | Where-Object { $_ -is [double] -and [double]::IsFinite($_) }
    )
    return $values
}

function New-TimeSyncReportObject {
    param(
        [ValidateSet('pre_sync','post_verify')][string]$ReportKind,
        [string]$NtpServer,
        [int]$SampleCount,
        [double[]]$OffsetsMs,
        [double]$ThresholdMs
    )
    if ($OffsetsMs.Count -lt 5) {
        throw "有效时间偏差样本不足，实际为 $($OffsetsMs.Count) 个"
    }
    $sorted = @($OffsetsMs | Sort-Object)
    $median = if ($sorted.Count % 2 -eq 1) {
        $sorted[[int]($sorted.Count / 2)]
    } else {
        ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2.0
    }
    $maxAbs = ($OffsetsMs | ForEach-Object { [math]::Abs($_) } | Measure-Object -Maximum).Maximum
    $status = if ($maxAbs -le $ThresholdMs) { "pass" } else { "fail" }
    $nowNs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000000L
    return [ordered]@{
        schema_version = 2
        report_kind = $ReportKind
        status = $status
        computer_name = $env:COMPUTERNAME
        ntp_server = $NtpServer
        checked_at_unix_ns = $nowNs
        sample_count = $SampleCount
        valid_sample_count = $OffsetsMs.Count
        median_offset_ms = [math]::Round($median, 6)
        max_abs_offset_ms = [math]::Round($maxAbs, 6)
        rtt_p50_ms = $null
        threshold_ms = $ThresholdMs
        windows_time_source = "$NtpServer,0x8"
    }
}
```

- [ ] **Step 3: 用夹具验证解析**

Run:

```powershell
. .\scripts\time-sync\Parse-Stripchart.ps1
$en = Get-Content .\scripts\time-sync\samples\stripchart-en.txt
$vals = Get-OffsetMillisecondsFromStripchart -Lines $en
if ($vals.Count -ne 5) { throw "en count" }
$zh = Get-Content .\scripts\time-sync\samples\stripchart-zh.txt
$valsZh = Get-OffsetMillisecondsFromStripchart -Lines $zh
if ($valsZh.Count -ne 5) { throw "zh count" }
$rep = New-TimeSyncReportObject -ReportKind pre_sync -NtpServer 192.168.10.20 -SampleCount 5 -OffsetsMs $vals -ThresholdMs 5
if ($rep.schema_version -ne 2) { throw "schema" }
Write-Output "parse ok"
```

Expected: `parse ok`

- [ ] **Step 4: 改写 `sync-windows-time.ps1` 产出 v2 pre_sync**

保留管理员检查、`w32tm /config`、`Restart-Service`、`/resync`；采样后调用共享函数；写出 JSON 含 `report_kind=pre_sync`、`schema_version=2`。

- [ ] **Step 5: 重写 `verify-windows-time.ps1` 为只读**

完整逻辑要点：

```powershell
# 禁止：调用 sync-windows-time.ps1、/config、Restart-Service、/resync
# 允许：w32tm /query /source、/query /status、stripchart
. (Join-Path $PSScriptRoot "Parse-Stripchart.ps1")
# 可选连通性：Test-NetConnection -ComputerName $NtpServer -Port 123 或 w32tm stripchart 失败即 fail
$raw = & w32tm /stripchart "/computer:$NtpServer" "/samples:$Samples" /dataonly 2>&1
$offsets = Get-OffsetMillisecondsFromStripchart -Lines @($raw)
$report = New-TimeSyncReportObject -ReportKind post_verify -NtpServer $NtpServer `
  -SampleCount $Samples -OffsetsMs $offsets -ThresholdMs $MaxOffsetMs
# 原子写出 JSON；status!=pass 则 exit 1
```

- [ ] **Step 6: 静态断言只读**

`scripts/time-sync/assert-verify-readonly.ps1`:

```powershell
$p = Join-Path $PSScriptRoot "verify-windows-time.ps1"
$text = Get-Content -Raw $p
$banned = @('sync-windows-time.ps1', '/config', 'Restart-Service', '/resync')
foreach ($b in $banned) {
    if ($text -match [regex]::Escape($b)) { throw "verify 脚本包含禁止内容: $b" }
}
Write-Output "verify readonly ok"
```

Run: `powershell -File scripts/time-sync/assert-verify-readonly.ps1`  
Expected: `verify readonly ok`

- [ ] **Step 7: Commit（audio-recoder）**

```bash
git add scripts/time-sync/
git commit -m "fix: 拆分只读时间复检并升级同步报告为 v2"
```

---

### Task 2: recorder 同步报告校验库（audio-recoder）

**Files:**
- Create: `src/timing/mod.rs`
- Create: `src/timing/sync_report.rs`
- Modify: `src/main.rs`（`mod timing;`，删除旧 `TimeSyncReport` 内联宽松逻辑）
- Modify: `src/capture/mod.rs`（`RecordConfig` 增加 `max_sync_report_age_secs: u64`，默认 600）
- Modify: CLI 解析增加 `--max-sync-report-age`

**Interfaces:**
- Produces:
  ```rust
  pub struct ValidatedTimeSyncReport {
      pub schema_version: u32, // 2
      pub report_kind: String, // pre_sync
      pub status: String,
      pub ntp_server: String,
      pub checked_at_unix_ns: i64,
      pub max_abs_offset_ms: f64,
      pub median_offset_ms: Option<f64>,
      pub rtt_p50_ms: Option<f64>,
  }
  pub fn load_and_validate_pre_sync(
      path: &Path,
      max_offset_ms: f64,
      max_age_secs: u64,
      now_unix_ns: i64,
  ) -> Result<ValidatedTimeSyncReport, String>;
  ```

- [ ] **Step 1: 写失败用例（单元测试放 `sync_report.rs` 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(json: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ts-report-{}.json", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        path
    }

    #[test]
    fn 拒绝_schema_v1() {
        let path = write_temp(r#"{"schema_version":1,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":100,"max_abs_offset_ms":1.0}"#);
        let err = load_and_validate_pre_sync(&path, 5.0, 600, 100).unwrap_err();
        assert!(err.contains("schema") || err.contains("版本"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 拒绝过期报告() {
        let path = write_temp(r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":0,"max_abs_offset_ms":1.0}"#);
        let now = 601_i64 * 1_000_000_000;
        let err = load_and_validate_pre_sync(&path, 5.0, 600, now).unwrap_err();
        assert!(err.contains("过期") || err.contains("有效期"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 拒绝缺_max_abs_offset() {
        let path = write_temp(r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"1.1.1.1","checked_at_unix_ns":100}"#);
        let err = load_and_validate_pre_sync(&path, 5.0, 600, 100).unwrap_err();
        assert!(!err.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 接受合法_pre_sync() {
        let path = write_temp(r#"{"schema_version":2,"report_kind":"pre_sync","status":"pass","ntp_server":"192.168.10.20","checked_at_unix_ns":1000000000,"max_abs_offset_ms":1.5,"median_offset_ms":1.0}"#);
        let rep = load_and_validate_pre_sync(&path, 5.0, 600, 1000000000).unwrap();
        assert_eq!(rep.ntp_server, "192.168.10.20");
        let _ = std::fs::remove_file(path);
    }
}
```

- [ ] **Step 2: Run 确认失败**

Run: `cargo test -q -- load_and_validate`  
Expected: 编译失败或测试失败（函数未实现）

- [ ] **Step 3: 实现 `load_and_validate_pre_sync`**

校验清单（全部失败返回中文 `Err`）：

1. JSON 反序列化  
2. `schema_version == 2`  
3. `report_kind == "pre_sync"`  
4. `status.to_lowercase() == "pass"`  
5. `ntp_server` 非空  
6. `max_abs_offset_ms` 存在、有限、`<= max_offset_ms`  
7. `checked_at_unix_ns` 存在；`(now_unix_ns - checked_at).abs()` 与 age：若 `now >= checked` 则 age_secs = (now-checked)/1e9，要求 `age_secs <= max_age_secs`；若 `checked > now + 60s` 也拒绝（时钟异常）

- [ ] **Step 4: Run 测试通过**

Run: `cargo test -q`  
Expected: PASS

- [ ] **Step 5: 接入 `main.rs` 的 `validate_time_sync`**

启用 `--timestamp-mark` 时调用 `timing::sync_report::load_and_validate_pre_sync`，把结果存入后续 sidecar。  
CLI 增加 `--max-sync-report-age`，写入 `RecordConfig.max_sync_report_age_secs`。

- [ ] **Step 6: Commit**

```bash
git add src/timing src/main.rs src/capture/mod.rs
git commit -m "feat: 强制校验 v2 同步报告 schema 与有效期"
```

---

### Task 3: QPC↔UTC 周期校准模块（audio-recoder）

**Files:**
- Create: `src/timing/qpc_utc.rs`
- Modify: `src/timing/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize)]
  pub struct QpcUtcCalibration {
      pub phase: String, // start | periodic | end
      pub qpc_100ns: u64,
      pub utc_unix_ns: i64,
      pub span_qpc_100ns: u64,
  }

  pub struct QpcUtcMapper {
      calibrations: Vec<QpcUtcCalibration>,
      clock_jump_detected: bool,
  }

  impl QpcUtcMapper {
      pub fn new() -> Self;
      /// Windows: 采 8 组取最短 span；非 Windows 测试可用 `push_for_test`
      pub fn capture(&mut self, phase: &str) -> Result<&QpcUtcCalibration, String>;
      pub fn push_for_test(&mut self, cal: QpcUtcCalibration);
      pub fn map_qpc_to_utc_ns(&self, qpc_100ns: u64) -> Result<i64, String>;
      pub fn clock_jump_detected(&self) -> bool;
      pub fn calibrations(&self) -> &[QpcUtcCalibration];
      fn detect_jump_against_last(&mut self, new: &QpcUtcCalibration);
  }
  ```

跳变规则（实现写死，与设计一致）：

- 若已有上一个校准点：`dqpc = new.qpc - old.qpc`（100ns 单位），`dutc = new.utc - old.utc`（ns）  
- 期望 `dutc_ns ≈ dqpc * 100`（因 qpc 字段已是 100ns ticks → ns 为 *100）  
- 若 `|dutc_ns - dqpc_100ns * 100| > 5_000_000`（5ms）→ jump  
- 或当 `dqpc_100ns > 0` 时，相对斜率 `|dutc/(dqpc*100) - 1.0| > 50e-6` 且该情况连续累计（第一版：单次超 50ppm 且偏差绝对值 > 1ms 也标 jump）

- [ ] **Step 1: 写纯逻辑测试（不依赖 WASAPI）**

```rust
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
        utc_unix_ns: 1_000_000 + 100_000, // 1000 * 100ns = 100us?  wait: 1000 ticks * 100 = 100_000 ns
        span_qpc_100ns: 1,
    });
    // qpc 1500 → 一半
    let utc = m.map_qpc_to_utc_ns(1_500).unwrap();
    assert_eq!(utc, 1_000_000 + 50_000);
}

#[test]
fn 大跳变标记 clock_jump() {
    let mut m = QpcUtcMapper::new();
    m.push_for_test(QpcUtcCalibration {
        phase: "start".into(),
        qpc_100ns: 1_000,
        utc_unix_ns: 0,
        span_qpc_100ns: 1,
    });
    m.push_for_test(QpcUtcCalibration {
        phase: "periodic".into(),
        qpc_100ns: 1_100, // +100 ticks = +10_000 ns 墙钟应约 +10us
        utc_unix_ns: 20_000_000, // 却跳了 20ms
        span_qpc_100ns: 1,
    });
    assert!(m.clock_jump_detected());
}
```

注意：`push_for_test` 必须调用与 `capture` 相同的 `detect_jump_against_last`。

- [ ] **Step 2: Run 确认失败 → 实现 → 通过**

Run: `cargo test -q -- qpc_utc`  
Expected: 先 FAIL/编译失败，实现后 PASS

- [ ] **Step 3: Windows `capture` 实现**

```rust
#[cfg(target_os = "windows")]
pub fn sample_once() -> Result<(u64 /*mid_qpc_100ns*/, i64 /*utc_ns*/, u64 /*span*/), String> {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    // 与现 main.rs qpc_to_utc_ns 相同：8 组取最短 span
    // midpoint_qpc ticks 转为 100ns： mid_100ns = mid_ticks * 10_000_000 / freq
    // filetime → unix_ns
}
```

非 Windows：`capture` 返回明确错误（timing 模式本就不支持）。

- [ ] **Step 4: Commit**

```bash
git add src/timing/qpc_utc.rs src/timing/mod.rs
git commit -m "feat: 增加 QPC/UTC 多点校准与跳变检测"
```

---

### Task 4: WASAPI Init 时机与真实设备选择（audio-recoder）

**Files:**
- Modify: `src/capture/wasapi_loopback.rs`
- Modify: `src/capture/mod.rs`（如需返回 `device_id`/`device_name`，可扩展 `StopHandle` 或另建 channel）

**Interfaces:**
- 行为变更：
  - `InitResult::Success` 仅在 `Start()` 成功后发送
  - `--device` 匹配到的 `IMMDevice` 用于 Activate，而不是仅 cpal 预检后仍 `GetDefaultAudioEndpoint`

- [ ] **Step 1: 调整成功通知位置**

将当前约 151–154 行的：

```rust
let _ = init_tx.send(InitResult::Success { ... });
```

移到 `audio_client.Start()` 成功之后。  
若 `Initialize`/`GetService`/`Start` 失败：`init_tx.send(InitResult::Failed(err))` 并 `return Err`。

- [ ] **Step 2: 实现按名解析 IMMDevice**

伪代码（Windows）：

```rust
unsafe fn resolve_render_device(
    enumerator: &IMMDeviceEnumerator,
    device_name: Option<&str>,
) -> Result<(IMMDevice, String /*friendly*/), String> {
    if device_name.is_none() {
        let d = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let name = friendly_name(&d).unwrap_or_else(|_| "default".into());
        return Ok((d, name));
    }
    let needle = device_name.unwrap().to_lowercase();
    let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
    // 遍历，读 PKEY_Device_FriendlyName，contains 匹配
    // 0 匹配 / >1 匹配 → Err 带列表
    // 返回唯一 IMMDevice
}
```

需要在 `Cargo.toml` windows features 中确认 `Win32_Media_Audio_Endpoints` / 属性键已启用；若缺 feature 按编译错误补。

删除“仅用 cpal 检查名称是否存在”的预检块，或保留为补充信息但不得替代 IMMDevice 选择。

- [ ] **Step 3: 日志打印实际设备**

```rust
eprintln!("[WASAPI] 使用输出设备: {friendly_name}");
```

- [ ] **Step 4: 手动冒烟（无自动化 WASAPI 设备时）**

Run: `cargo check --all-targets`  
Expected: 无错误

可选：`cargo run -- --list-devices` 后对已知名称 `--device` 短录 1s（人工）。

- [ ] **Step 5: Commit**

```bash
git add src/capture/wasapi_loopback.rs Cargo.toml
git commit -m "fix: WASAPI 初始化成功时机与真实设备选择"
```

---

### Task 5: v2 sidecar 结构、sha256 与原子写出（audio-recoder）

**Files:**
- Create: `src/timing/sidecar.rs`
- Create: `src/timing/sha256.rs`（推荐加依赖 `sha2 = "0.10"`，实现简单）
- Modify: `Cargo.toml`
- Modify: `src/main.rs` `write_recording` 全流程

**Interfaces:**
- Produces:
  ```rust
  pub fn sha256_hex(bytes: &[u8]) -> String;
  pub fn write_wav_and_sidecar_atomic(
      wav_path: &Path,
      wav_bytes_or_write_fn: /* 现有 hound 写完后读回或边写边 hash */,
      sidecar: &TimingSidecarV2,
  ) -> Result<(), String>;
  ```

`TimingSidecarV2` 字段必须与设计文档 4.2 一致（`schema_version: 2`，含 `wav_sha256`、`qpc_utc_calibrations`、`clock_jump_detected`、`recording_started_unix_ns`、`recording_ended_unix_ns`、完整 `time_sync`）。

- [ ] **Step 1: 加依赖并实现 sha256**

`Cargo.toml`:

```toml
sha2 = "0.10"
```

```rust
// src/timing/sha256.rs
use sha2::{Digest, Sha256};
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
#[test]
fn empty_sha() {
    // echo -n | sha256sum
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
```

- [ ] **Step 2: 原子写出逻辑测试（用临时目录）**

```rust
#[test]
fn sidecar_失败时不留下成功 wav() {
    // 写 partial wav 成功，然后模拟 sidecar 序列化到非法路径或手动制造失败
    // 断言最终 wav 不存在
}
```

实现约定：

1. 删除已存在的 `out.wav.timing.json`（避免旧配对）  
2. 写 `out.wav.partial`  
3. 读 partial 算 sha256，填入 sidecar  
4. 写 `out.wav.timing.json.partial`  
5. 若 `clock_jump_detected`：仍可写文件但应在 sidecar 标明 true，且进程退出码非 0（正式验收禁用）；或直接 Err 不产出——**采用：产出 sidecar 但 `main` 返回 Err("检测到墙钟跳变") 且删除已 rename 的文件**，更安全。  
   **最终采用（与设计一致、便于调试）：** 若 jump 或写失败 → 删除 partial/最终产物，返回 Err。无 jump 时 rename：先 `timing.json.partial → timing.json`，再 `wav.partial → wav`。  
6. 任一步失败清理所有 partial 与本次写出目标。

- [ ] **Step 3: 重写 `write_recording` 使用 mapper**

流程：

```text
recording_started = now_ns
mapper.capture("start")
（packets 已在外部采完的当前结构下：）
对每个 packet：utc = mapper.map_qpc_to_utc_ns(qpc)
构建 anchors（首包 wav_index=0）
若 duration 允许，录制循环中应 periodic capture——见 Step 3b
mapper.capture("end")
若 jump → Err
写 FSK 用 first_pcm 时间
原子写出
```

**Step 3b — 周期校准接入点：**  
当前架构是“录完再 write”。为满足设计，在 `run()` 收包循环中每 ≥1s 调用共享 `Arc<Mutex<QpcUtcMapper>>.capture("periodic")`，或在收包循环用上次校准时间判断。  
最小改动：`run` 循环内：

```rust
let mut mapper = QpcUtcMapper::new();
mapper.capture("start")?;
let mut last = Instant::now();
// 每收到 packet 或 sleep 时：
if last.elapsed() >= Duration::from_secs(1) {
    let _ = mapper.capture("periodic");
    last = Instant::now();
}
// stop 后
mapper.capture("end")?;
write_recording(..., mapper, validated_sync, ...)?;
```

- [ ] **Step 4: 删除旧 `qpc_to_utc_ns` 一次性回填**

`main.rs` 中旧函数删除或仅作 Windows 底层采样被 `qpc_utc` 调用。

- [ ] **Step 5: `cargo test` + `cargo check --all-targets`**

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/timing src/main.rs
git commit -m "feat: v2 sidecar 原子写出与录制期 QPC 校准接入"
```

---

### Task 6: checker timing v2 单端校验（audio-checker）

**Files:**
- Modify: `D:\code\audio-checker\src\timing.rs`（重写结构体与 `load_and_validate`）
- Modify: `D:\code\audio-checker\tests\integration_tests.rs` 中的 `write_sidecar` 辅助函数升 v2

**Interfaces:**
- `TimingSidecar` 增加字段：`wav_file`, `wav_sha256`, `recording_started_unix_ns`, `recording_ended_unix_ns`, `qpc_utc_calibrations`, `clock_jump_detected`, `actual_device_sample_rate`（可选）
- `TimeSyncMetadata` 增加：`schema_version`, `report_kind`, `checked_at_unix_ns`, `median_offset_ms`, `rtt_p50_ms`；`server` 改为必须非空 `String`（或校验时当空失败）
- `load_and_validate(path, wav_path, sample_rate, sample_count) -> Result<ValidatedTiming, String>`  
  **签名变更：** 增加 `wav_path` 以便 basename + sha256

- [ ] **Step 1: 改 `write_sidecar` 辅助生成 v2**

```rust
fn write_sidecar(path: &Path, sample_rate: u32, clock_start_ns: i64, server: &str) {
    let bytes = fs::read(path).unwrap();
    let hash = {
        use std::collections::hash_map::DefaultHasher; // 不要用这个
        // 与生产一致：在测试里手写 sha256 或复制算法
        sha256_hex_for_test(&bytes)
    };
    // schema_version: 2
    // wav_file: path.file_name()
    // wav_sha256: hash
    // clock_jump_detected: false
    // recording_started_unix_ns: clock_start_ns - 1_000_000_000
    // recording_ended_unix_ns: clock_start_ns + 9e9
    // qpc_utc_calibrations: [start, end]
    // anchors 首 index 0，四字段单调
    // time_sync: schema_version 2, report_kind pre_sync, status pass, server, checked_at, max_abs_offset_ms
}
```

若 checker 不想加 `sha2` 依赖：在 `timing.rs` 内对 WAV 文件自行 `sha2`，测试 crate 可加 dev-dependency；**推荐 checker 也加 `sha2 = "0.10"`**。

- [ ] **Step 2: 实现校验（按设计 7.2）**

顺序失败即返回中文错误。关键片段：

```rust
if sidecar.schema_version != 2 { return Err(...); }
if sidecar.clock_jump_detected { return Err("检测到墙钟跳变".into()); }
if !sidecar.discontinuities.is_empty() { return Err(...); }
let expected_name = wav_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
if sidecar.wav_file != expected_name && Path::new(&sidecar.wav_file).file_name().and_then(|s| s.to_str()) != Some(expected_name) {
    return Err(format!("wav_file 与输入不一致"));
}
let actual_hash = sha256_file(wav_path)?;
if !sidecar.wav_sha256.eq_ignore_ascii_case(&actual_hash) {
    return Err("wav_sha256 与文件内容不一致".into());
}
// anchors 单调：wav_sample_index, device_position, qpc_100ns, utc_unix_ns
// anchors[0].wav_sample_index == 0
// calibrations 含 start/end
// time_sync 全字段
// checked_at 与 recording_started 间隔 <= 600s
// 斜率：相邻 anchor |sample_rate_fit / sample_rate - 1| <= 0.01
```

- [ ] **Step 3: 更新所有调用 `load_and_validate` 处**

`analyze.rs` `prepare`：

```rust
let timing = timing::load_and_validate(timing_path, path, audio.sample_rate, audio.samples.len())?;
```

- [ ] **Step 4: 先让旧集成测试按 v2 绿**

Run: `cargo test -q`（在 `D:\code\audio-checker`）  
Expected: 原成功用例 PASS；若有仍写 v1 的测试，改为 v2 或改为“拒绝 v1”。

- [ ] **Step 5: Commit（audio-checker）**

```bash
git add src/timing.rs src/analyze.rs tests/integration_tests.rs Cargo.toml Cargo.lock
git commit -m "feat: checker 仅接受 v2 sidecar 并校验绑定字段"
```

---

### Task 7: checker FSK 对照、成对规则与误差强制（audio-checker）

**Files:**
- Modify: `src/analyze.rs`
- Modify: `src/timing.rs`（如导出 UTC 日计算）

- [ ] **Step 1: FSK 对照**

在 `prepare` 中读入完整 WAV samples 后：

```rust
let decoded = crate::timestamp::decode(&audio.samples, audio.sample_rate)
    .ok_or_else(|| AnalysisFailure::new("UNSUPPORTED_TIMING_PROTOCOL", "无法解码 FSK 时间标记"))?;
let delta = (decoded.millis_of_day as i64 - timing.sidecar.first_pcm_millis_of_day as i64).abs();
if delta > 1 {
    return Err(AnalysisFailure::new(
        "UNSUPPORTED_TIMING_PROTOCOL",
        format!("FSK 与 sidecar first_pcm_millis_of_day 差 {delta}ms，超过 1ms"),
    ));
}
```

注意：若 FSK 前有静音，使用 `decode_with_offset` 并确认 marker 终点与 `fsk_prefix_samples` 一致（允许小差异则严格：`decoded.marker_samples` 与 sidecar 前缀比较，不一致则拒绝或 warning——**第一版要求 `marker_samples == fsk_prefix_samples` 或差为 0**）。

- [ ] **Step 2: 成对规则**

```rust
// server 相同
// relative_bound = abs(s) + abs(r)；两者 max_abs 必须 Some，否则拒绝
// 同一 UTC 日：
fn utc_day(ns: i64) -> i64 { ns.div_euclid(86_400_000_000_000) } // 错误：应用 86_400 * 1e9
fn utc_day(ns: i64) -> i64 { ns.div_euclid(86_400 * 1_000_000_000) }
if utc_day(sender.first_pcm) != utc_day(receiver.first_pcm) { 拒绝跨午夜 }
```

- [ ] **Step 3: 输出**

```rust
report.timing_mode = "sidecar-anchors-v2".into();
// clock_quality 填 checked_at
```

- [ ] **Step 4: 失败路径集成测试（每个独立 `#[test]`）**

在 `tests/integration_tests.rs` 增加：

| 测试名 | 做法 | 期望 status/error |
|---|---|---|
| 缺 sidecar | 不写 timing | UNSUPPORTED |
| schema v1 | schema_version=1 | UNSUPPORTED |
| hash 不符 | 改 sidecar hash | UNSUPPORTED |
| FSK 不一致 | first_pcm_millis 改 +5 | UNSUPPORTED |
| 过期 checked_at | checked_at 很旧 | UNSUPPORTED |
| 缺 max_abs | 去掉字段 | UNSUPPORTED |
| server 不同 | 两端不同 server | UNSUPPORTED |
| 误差超限 | offset 各 6ms，max_clock_error=10 | 上界 12>10 拒绝 |
| 跨日 | first_pcm 差 2 天 | UNSUPPORTED |
| QPC 非单调 | 颠倒 qpc | UNSUPPORTED |
| clock_jump | true | UNSUPPORTED |

成功用例断言 `timing_mode == "sidecar-anchors-v2"`。

- [ ] **Step 5: Run**

```bash
cd /d/code/audio-checker && cargo test -q
```

Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/analyze.rs src/timing.rs tests/integration_tests.rs
git commit -m "feat: FSK 对照与成对时钟/跨午夜强制校验"
```

---

### Task 8: chrony 脚本复核与现场清单（audio-recoder）

**Files:**
- Modify: `scripts/time-sync/install-chrony-server.sh`（仅当与设计不符时改；保持幂等 managed block）
- Modify: `scripts/time-sync/verify-chrony-server.sh`
- Create: `docs/field-acceptance-checklist-v2.md`
- Modify: `docs/2026-07-18-offline-ntp-audio-latency-implementation-status.md`（标记 v2 进行中/待现场）

- [ ] **Step 1: 核对 install/verify 与设计 6.2**

确认：`local stratum`、managed block、默认 allow、firewalld/ufw、非 root 失败、`--dry-run`。缺则补。

- [ ] **Step 2: 写现场清单 `docs/field-acceptance-checklist-v2.md`**

必须包含：

1. 一次性 chrony 安装命令  
2. 每端 pre_sync / 录制 / post_verify / checker 命令（v2）  
3. 归档文件列表  
4. 作废条件  
5. 10 轮记录表（轮次、P50、预/后偏差、是否有效）  
6. 通过标准表（5ms / 10ms / 10 轮）

- [ ] **Step 3: 更新 status 文档**

明确：

- 代码目标：v2  
- 现场验收：未完成  
- 旧 v1 不再支持  

- [ ] **Step 4: Commit**

```bash
git add scripts/time-sync docs/
git commit -m "docs: 现场验收清单与 chrony 脚本复核"
```

---

### Task 9: 跨仓库契约冒烟与收尾

**Files:**
- 可能微调字段名不一致处（以设计文档为准）
- 两边 `cargo test` / `cargo check`

- [ ] **Step 1: 对照设计文档字段表，grep 两边代码**

```bash
# recorder
rg "schema_version|report_kind|wav_sha256|qpc_utc_calibrations|clock_jump" src scripts
# checker
rg "schema_version|report_kind|wav_sha256|qpc_utc_calibrations|clock_jump" src tests
```

不一致则修到一致。

- [ ] **Step 2: 全量测试**

```bash
cd /d/code/audio-recoder && cargo test -q && cargo check --all-targets
cd /d/code/audio-checker && cargo test -q && cargo check --all-targets
powershell -File D:/code/audio-recoder/scripts/time-sync/assert-verify-readonly.ps1
```

Expected: 全 PASS

- [ ] **Step 3: 若有字段修复，分别 commit**

```bash
# 各仓库
git commit -m "fix: 对齐 v2 契约字段名与校验"
```

- [ ] **Step 4: 交付给用户现场验收**

交给用户：

- 设计文档  
- 本计划完成状态  
- `docs/field-acceptance-checklist-v2.md`  
- 两端 release 构建说明：`cargo build --release`

---

## Self-Review（对照 spec）

| Spec 章节 | 对应 Task |
|---|---|
| 4.1 同步报告 v2 | Task 1, 2 |
| 4.2 sidecar v2 | Task 5, 6 |
| 4.3 FSK 语义 | Task 5, 7 |
| 4.4 删除 v1 | Task 2, 5, 6, 7 |
| 5 recorder Init/设备 | Task 4 |
| 5 QPC 周期映射 | Task 3, 5 |
| 5 原子写出/绑定 | Task 5 |
| 6 脚本拆分/chrony | Task 1, 8 |
| 7 checker 校验/插值 | Task 6, 7 |
| 8 测试矩阵 | Task 1, 2, 3, 6, 7, 9 |
| 9 现场验收 | Task 8, 9（执行在用户侧） |
| P1-1…P1-5 / P2-1…P2-4 | 分别落入 Task 3/5, 6/7, 2, 1, 4, 4, 6, 7, 7 |

无 TBD。类型名以各 Task Interfaces 为准：`ValidatedTimeSyncReport`、`QpcUtcMapper`、`TimingSidecarV2`（recorder 侧）/ `TimingSidecar`（checker 侧，字段同构）。

---

## 执行说明

实现时严格 TDD：先测后码，每 Task 结束 commit。  
两仓库交替：建议 Task 1–5（recoder）→ Task 6–7（checker）→ Task 8–9。  
现场 10 轮不在 agent 环境完成；代码全部 Task 绿后交用户按清单验收。
