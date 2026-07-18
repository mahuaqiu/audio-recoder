# 离线 NTP 音频时延方案实现状态

## 当前状态（2026-07-19 v2）

- 协议已升级为 schema_version=2（不兼容 v1）。
- 代码侧：recorder 周期 QPC 校准、Init 时机、只读 verify、sidecar 绑定；checker 强制绑定/FSK/跨午夜/误差字段。
- 现场 10 轮双机验收尚未完成，完成前不得标记任务正式完成。
- 操作清单见 docs/field-acceptance-checklist-v2.md。
- 设计：docs/superpowers/specs/2026-07-19-offline-ntp-audio-latency-v2-design.md
- 实现计划：docs/superpowers/plans/2026-07-19-offline-ntp-audio-latency-v2.md

## 已验证（自动化）

- audio-recoder worktree：cargo test 通过（含 sync_report / qpc_utc / sha256 / sidecar 单测）。
- audio-checker feature/offline-ntp-v2：cargo test 通过（单元 + v2 集成失败路径）。

## 现场验收限制

本地无法代替真实双机与会议链路；需按清单完成至少 10 轮后更新本文件为现场通过。