# 离线 NTP 音频时延方案审查记录

## 1. 审查结论

审查日期：2026-07-19

涉及仓库：

- `D:\code\audio-recoder`
- `D:\code\audio-checker`

当前不能认定该任务已经完成。两个仓库的核心链路已经完成初版编码，常规编译和现有自动化测试通过，但仍有会影响时间准确性、元数据完整性和现场验收可信度的问题。

当前建议状态：

> 阶段 1 至阶段 4 已完成初版实现，尚未达到正式验收条件；阶段 5 双机现场验收未完成。

本次审查没有修改业务代码，只新增本审查文档。

## 2. 已完成内容

### 2.1 `audio-recoder`

- 已增加 chrony 服务安装和检查脚本。
- 已增加 Windows 时间同步脚本和验证脚本。
- WASAPI loopback packet 已携带 `device_position`、QPC 时间和 buffer flags。
- 已切换新 FSK 语义：FSK 数值表示 FSK 前缀之后第一帧真实 PCM 的统一墙钟毫秒时间。
- 启用 `--timestamp-mark` 时要求提供时间同步报告。
- 已生成 WAV 旁路 timing 文件：`<wav>.timing.json`。
- 已尝试依据设备位置拼接 PCM，并对设备位置缺口、buffer flags 写入 discontinuity 信息。
- 已支持目标采样率重采样。

### 2.2 `audio-checker`

- 已支持默认查找 `<wav>.timing.json`。
- 已支持 `--sender-timing` 和 `--receiver-timing` 显式指定 sidecar。
- 已校验 schema、FSK 语义、时钟域、WAV 采样率、anchor 单调性和 discontinuity。
- 已根据 anchors 将事件样点插值到 UTC 时间。
- 已拒绝缺少 sidecar、协议不匹配、同步失败和存在 discontinuity 的输入。
- 已输出 timing mode 和相对时钟误差上界。
- 已覆盖 16 kHz、48 kHz、缺少 sidecar、NTP server 不一致、事件数量不一致和时延超限等基础集成场景。

## 3. 需要修复的问题

以下问题按优先级排序。P1 表示会影响正式结果或可能让无效数据通过；P2 表示重要功能缺口或现场误操作风险。

### P1-1：QPC 到 UTC 映射没有按录制过程采集

位置：

- `audio-recoder/src/main.rs:314`
- `audio-recoder/src/main.rs:328`
- `audio-recoder/src/main.rs:522`

当前实现保存了每个 WASAPI packet 的 QPC，但在录制结束写文件时，才使用当时读取到的系统时间建立 QPC/UTC 映射。计划要求的是启动时、录制过程中周期性、结束时采集 QPC 与 UTC 的对应关系。

影响：

- 不能检测录制期间墙钟跳变。
- W32Time 在录制期间调整时，历史 QPC 可能被错误映射。
- sidecar 中的多个 anchor 并不是独立的 QPC/UTC 校准点，只是使用同一映射换算出的 packet 时间。

建议：

- 启动时采集多组 `QPC before -> GetSystemTimePreciseAsFileTime -> QPC after`，选择最短区间。
- 录制期间按约 1 秒周期采集新的映射点。
- 结束时再次采集多组映射点。
- 检测墙钟映射突变或不合理斜率，并在 sidecar 中标记失败，禁止 checker 使用。

### P1-2：checker 没有确认 sidecar 属于当前 WAV

位置：

- `audio-checker/src/timing.rs:8`
- `audio-checker/src/analyze.rs:231`

checker 没有校验 sidecar 中的 `wav_file`，也没有解码 WAV 内的 FSK 并与 `first_pcm_millis_of_day` 对照。当前主要依据 sidecar 的 `fsk_prefix_samples` 截取真实 PCM。

影响：

- 可以把另一段录音的 sidecar 显式传给当前 WAV。
- 只要采样率和结构兼容，checker 可能接受错误的时间轴并输出错误时延。
- recorder 直接覆盖 WAV，sidecar 写入失败时旧 sidecar 可能继续存在，形成旧 WAV/新 sidecar 或新 WAV/旧 sidecar 的配对风险。

建议：

- 校验 sidecar 的 `wav_file` 与当前 WAV 的规范化路径，或增加 WAV 内容 hash/文件标识。
- checker 解码 FSK，验证其数值与 `first_pcm_millis_of_day` 一致，允许范围按计划控制在 1 ms 内。
- recorder 写 WAV 和 sidecar 前清理旧 sidecar，或使用临时 WAV 与临时 sidecar，完成后按明确顺序原子替换。
- sidecar 写入失败时删除或标记对应 WAV，避免产生看似完整的成功产物。

### P1-3：同步报告没有时效校验

位置：

- `audio-recoder/src/main.rs:268`
- `audio-checker/src/timing.rs:39`
- `audio-checker/src/analyze.rs:160`

recorder 只检查同步报告的 `status` 和偏差值，没有检查：

- `schema_version` 是否受支持。
- `ntp_server` 是否存在且有效。
- `checked_at_unix_ns` 距离录制开始是否在允许窗口内。

checker 没有读取或校验 `checked_at_unix_ns`。如果两边的 `max_abs_offset_ms` 缺失，相对误差上界会变成 `None`，但分析仍可能继续。

影响：

- 几天前生成的 `pass` 报告可以继续用于录音。
- 缺失偏差字段的 sidecar 可能绕过 `--max-clock-error`。
- 无法保证报告确实代表本次测试前的同步状态。

建议：

- recorder 严格校验同步报告 schema、server、状态、偏差字段和时间戳。
- 定义并实现报告有效期，例如录制开始前不超过 10 分钟。
- checker 对两端报告都要求存在有效的时间戳、server、偏差和状态。
- 缺失任一关键字段时返回 `UNSUPPORTED_TIMING_PROTOCOL`。

### P1-4：录制后验证脚本实际会重新校时

位置：

- `audio-recoder/scripts/time-sync/verify-windows-time.ps1:10`

`verify-windows-time.ps1` 直接调用 `sync-windows-time.ps1`。后者会重新配置 W32Time、重启服务并执行 `/resync`，因此不是计划要求的“只检查、不修改配置”的录制后复检。

影响：

- 录制期间发生的时钟漂移可能在复检前被重新同步掩盖。
- 录制前后报告不再能有效证明录制期间的时间基准稳定。

建议：

- 将校时和只读验证拆成两个独立脚本。
- 只读验证不得调用 `/config`、`Restart-Service`、`/resync`。
- 只读验证仅执行连通性、`w32tm /query` 和 `stripchart` 采样，并生成独立报告。

### P1-5：WASAPI 尚未真正启动就报告初始化成功

位置：

- `audio-recoder/src/capture/wasapi_loopback.rs:151`
- `audio-recoder/src/capture/wasapi_loopback.rs:156`

录音线程在调用 `IAudioClient::Initialize`、`GetService` 和 `Start` 之前，就向主线程发送 `InitResult::Success`。

影响：

- 设备初始化失败时，前台调用方可能已经进入录制等待。
- 后台模式可能误报“后台录音已启动”。
- 设备独占、权限或 WASAPI 启动错误不能可靠地传递给调用方。

建议：

- 将成功通知移动到 `Initialize`、`GetService`、`Start` 全部成功之后。
- 录音线程应将初始化错误通过专用 channel 传回主线程。
- 增加初始化失败时前台和后台模式退出码、输出文件和 sidecar 状态的测试。

## 4. 其他重要问题

### P2-1：`--device` 不会真正选择指定的扬声器

位置：

- `audio-recoder/src/capture/wasapi_loopback.rs:84`
- `audio-recoder/src/capture/wasapi_loopback.rs:125`

当前逻辑只使用 cpal 检查设备名称是否存在，但实际仍然调用 `GetDefaultAudioEndpoint(eRender, eConsole)`。指定非默认设备时，程序可能继续录制默认扬声器。

建议将匹配到的 `IMMDevice` 传递给 WASAPI 初始化，而不是只做名称预检查；同时增加指定设备后确认实际设备 ID/名称的诊断输出。

### P2-2：checker 对 anchor 元数据的校验不完整

位置：

- `audio-checker/src/timing.rs:25`
- `audio-checker/src/timing.rs:112`

当前只检查 `wav_sample_index` 和 `utc_unix_ns` 单调，没有检查：

- `device_position` 是否单调。
- `qpc_100ns` 是否单调。
- 第一个 anchor 是否对应真实 PCM 起点。
- anchor 是否落在 FSK 前缀之后的真实 PCM 区间。
- anchor 之间的采样率/时间斜率是否合理。

建议增加结构一致性校验，拒绝明显异常的设备位置、QPC 或采样率拟合结果。

### P2-3：没有实现计划中的跨午夜和同日校验

计划要求第一版对跨午夜录音明确拒绝或警告，但当前 checker 没有根据 `first_pcm_utc_unix_ns` 校验两端日期，也没有防止当天毫秒值跨午夜造成 FSK 语义歧义。

建议在 checker 中加入 UTC 日期检查；跨日期时返回明确的 `UNSUPPORTED_TIMING_PROTOCOL`，除非后续协议明确支持跨午夜。

### P2-4：当前测试没有覆盖关键失败路径

现有测试覆盖了理想 sidecar 和部分基础错误，但尚未覆盖：

- sidecar 与 WAV 不匹配。
- FSK 与 sidecar 时间不一致。
- 过期同步报告。
- 缺少 `max_abs_offset_ms`。
- 缺少或无效的 `ntp_server`。
- 两端录音日期不同。
- `device_position` 或 QPC 非单调。
- anchor 落在 FSK 前缀内。
- 录制中墙钟跳变。
- WAV 写入成功、sidecar 写入失败。
- WASAPI 初始化或启动失败。
- 指定设备不是默认扬声器的情况。

## 5. 已执行验证

### `audio-recoder`

- `cargo test`：通过，4 个测试。
- `cargo check --all-targets`：通过。

### `audio-checker`

- `cargo test`：通过，12 个单元测试和 6 个集成测试。
- `cargo check --all-targets`：通过。

这些结果只能证明当前代码能够编译，并通过现有的理想数据测试，不能证明已经满足双机精确时延测量的完成定义。

## 6. 尚未完成的验收项

以下项目没有在当前本地环境中完成：

- Linux 宿主机实际安装、重启后 chrony 自动启动和回滚验证。
- Windows 两台真实设备的 WASAPI loopback `device_position`/QPC 诊断。
- QPC 到 UTC 映射误差和墙钟跳变检测。
- 会议软件实际采集、编码、传输、播放链路测试。
- chrony UDP/123 的局域网连通性和防火墙验证。
- 同机回环重复测量统计。
- 双机固定链路至少 10 轮现场测量。
- P95、P50、最小值、最大值及误差预算汇总。

## 7. 建议修复顺序

1. 修复 QPC/UTC 映射模型，增加录制期间时钟跳变检测。
2. 修复 recorder 初始化成功通知和指定设备选择问题。
3. 将录制后只读复检从重新同步脚本中拆出。
4. 增加同步报告 schema、时间有效期和关键字段校验。
5. 绑定 WAV 与 sidecar，并验证 FSK 与 sidecar 的一致性。
6. 补充 anchor、跨午夜、旧 sidecar、过期报告和写入失败测试。
7. 在真实 Linux/Windows 双机环境完成现场验收。
8. 根据现场数据更新实现状态文档，只有满足全部完成定义后再标记任务完成。

