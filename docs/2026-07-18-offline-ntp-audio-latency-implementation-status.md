# 离线 NTP 音频时延方案实现状态

## 已完成

- 宿主机脚本：`scripts/time-sync/install-chrony-server.sh`、`verify-chrony-server.sh`。
- 宿主机安装脚本未指定 `--allow-cidr` 时默认允许所有 IPv4/IPv6 来源访问 UDP/123；显式指定后可限制为指定网段。
- Windows 校时脚本：`scripts/time-sync/sync-windows-time.ps1`、`verify-windows-time.ps1`。
- recorder 的 WASAPI loopback packet 携带 `device_position`、QPC 100ns 时间和 buffer flags。
- 新 FSK 语义：FSK 数值表示 WAV 中 FSK 前缀之后第一帧真实 PCM 的统一墙钟毫秒时间。
- recorder 在启用 `--timestamp-mark` 时强制读取并校验 `--time-sync-report`，生成 WAV 和 `.timing.json`。
- recorder 使用设备位置拼接 PCM，再做一次全局重采样；设备位置缺口、WASAPI 数据断流和时间戳错误会写入 sidecar。
- checker 默认读取 `<wav>.timing.json`，也支持 `--sender-timing` 和 `--receiver-timing`。
- checker 只接受 `schema_version=1`、`fsk_semantics=first_pcm_sample`、WASAPI loopback 和通过的 time sync 报告。
- checker 使用 anchors 对事件样点插值到 UTC，不再把 FSK 前缀额外加到事件时间。
- 缺少 sidecar、旧协议、同步服务端不一致、同步失败或存在 discontinuity 时返回 `UNSUPPORTED_TIMING_PROTOCOL`。

## 使用方式

两台 Windows 电脑分别执行，使用同一台宿主机的局域网 IP：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\time-sync\sync-windows-time.ps1 `
  -NtpServer 192.168.10.20 `
  -Samples 10 `
  -MaxOffsetMs 5 `
  -OutputPath .\time-sync-report.json
```

同步报告通过后，两台电脑分别启动扬声器录音。接收端只需把输出路径改成 `receiver.wav`：

```powershell
.\audio-recorder.exe `
  --source speaker `
  --sample-rate 16000 `
  --sample-fmt s16 `
  --duration 120 `
  --output .\sender.wav `
  --timestamp-mark `
  --time-sync-report .\time-sync-report.json `
  --require-time-sync `
  --blocking
```

两台录音不要求同时按下回车，建议都提前开始录音，确认稳定后再播放固定测试音频。

分析命令：

```powershell
.\audio-checker.exe `
  --sender .\sender.wav `
  --receiver .\receiver.wav `
  --count 5 `
  --max-clock-error 10 `
  --pretty
```

如果 sidecar 不在默认位置，可显式指定 `--sender-timing` 和 `--receiver-timing`。

## 已验证内容

- `D:\code\audio-recoder`：`cargo check`、`cargo test` 通过。
- `D:\code\audio-checker`：`cargo check`、`cargo test` 通过。
- checker 集成测试覆盖新 sidecar、48kHz 输入、缺 sidecar、NTP server 不一致和时延超限。

## 现场验收限制

本地环境没有两台真实 Windows WASAPI 扬声器设备，因此以下内容需要现场验收：WASAPI loopback 返回的 device position/QPC、QPC 到 Windows UTC 的映射误差、会议软件处理链路和 chrony UDP/123 局域网连通性。

出现 WASAPI discontinuity 时不要把该轮结果作为正式时延结果，应重新校时并重新录制。
