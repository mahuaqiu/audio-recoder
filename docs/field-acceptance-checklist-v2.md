# 现场验收清单（离线 NTP 采播时延 v2）

## 一次性部署

1. 在 Linux 宿主机安装 chrony：
   ```bash
   sudo bash scripts/time-sync/install-chrony-server.sh --bind-address <宿主机IP>
   sudo bash scripts/time-sync/verify-chrony-server.sh
   ```
2. 两台 Windows 部署 audio-recorder、audio-checker 与 scripts/time-sync/。
3. 固定会议软件音频配置（设备、降噪、AGC）并记录。

## 每轮测试（至少 10 轮有效）

在两端分别执行：

```powershell
# 1. 录制前校时（管理员）
.\scripts\time-sync\sync-windows-time.ps1 `
  -NtpServer <宿主机IP> -Samples 10 -MaxOffsetMs 5 `
  -OutputPath .\time-sync-report.json

# 2. 提前启动录音
.\audio-recorder.exe -s speaker -r 16000 -f s16 -d 120 -o .\sender.wav `
  -t --time-sync-report .\time-sync-report.json --require-time-sync -b
# 接收端把 -o 改为 receiver.wav

# 3. 播放固定测试音（至少 5 事件，间隔 3 至 5 秒）

# 4. 停止录音后只读复检
.\scripts\time-sync\verify-windows-time.ps1 `
  -NtpServer <宿主机IP> -Samples 10 -MaxOffsetMs 5 `
  -OutputPath .\time-sync-post-report.json

# 5. 分析
.\audio-checker.exe --sender .\sender.wav --receiver .\receiver.wav --count 5 --pretty
```

## 归档

每轮保存：两端 WAV、.timing.json、pre_sync/post_verify 报告、checker JSON。

## 作废条件

- pre 或 post 非 pass
- recorder/checker 失败
- discontinuity / clock_jump
- 相对误差上界大于 10 ms
- 事件数不对齐

## 通过标准

| 指标 | 目标 |
|---|---:|
| 每台相对 NTP | 不超过 5 ms（pre 与 post） |
| 相对误差上界 | 不超过 10 ms |
| 有效轮次 | 至少 10 |
| timing_mode | sidecar-anchors-v2 |

## 结果表

| 轮次 | pre s/r | post s/r | P50 时延 | 有效 | 备注 |
|---|---|---|---|---|---|
| 1 |  |  |  |  |  |
