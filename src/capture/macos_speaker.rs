use crate::capture::RecordConfig;
use crate::capture::StopHandle;
use std::sync::mpsc;

/// macOS 扬声器录制（stub 实现）
/// 注意：macOS 上录制扬声器需要更复杂的实现
/// 当前版本仅支持麦克风录制
pub fn record_speaker(
    _config: &RecordConfig,
    _tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    Err("macOS 扬声器录制功能暂未实现，请使用麦克风录制 (--source microphone)".into())
}
// 注意：即使返回 Err，StopHandle 类型签名必须与 microphone 模块一致
// 这样 main.rs 中 match 两个分支的类型才能统一
