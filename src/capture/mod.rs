mod microphone;

#[cfg(target_os = "windows")]
mod wasapi_loopback;

#[cfg(target_os = "macos")]
mod macos_speaker;

pub use microphone::record_microphone;

#[cfg(target_os = "windows")]
pub use wasapi_loopback::record_speaker;

#[cfg(target_os = "macos")]
pub use macos_speaker::record_speaker;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 录制停止句柄，drop 时停止录制
pub struct StopHandle {
    /// cpal 音频流，用于麦克风录制
    stream: Option<cpal::Stream>,
    /// 停止标志，用于扬声器录制
    stop_flag: Option<Arc<AtomicBool>>,
}

impl StopHandle {
    /// 创建麦克风录制的停止句柄
    pub fn new_microphone(stream: cpal::Stream) -> Self {
        Self {
            stream: Some(stream),
            stop_flag: None,
        }
    }

    /// 创建扬声器录制的停止句柄
    pub fn new_speaker(stop_flag: Arc<AtomicBool>) -> Self {
        Self {
            stream: None,
            stop_flag: Some(stop_flag),
        }
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        // 1. 如��有 cpal::Stream，drop 会自动停止音频流（麦克风）
        if self.stream.is_some() {
            self.stream = None; // 这会 drop stream
        }
        
        // 2. 如果有停止标志，设置它来通知录制线程（扬声器）
        if let Some(flag) = self.stop_flag.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// 音频源类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    Microphone,
    Speaker,
}

/// 录制配置
#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub source: Source,
    pub sample_rate: u32,
    pub sample_fmt: SampleFmt,
    pub duration_secs: u64,
    pub output_path: String,
}

impl Default for RecordConfig {
    fn default() -> Self {
        RecordConfig {
            source: Source::Microphone,
            sample_rate: 16000,
            sample_fmt: SampleFmt::S16,
            duration_secs: 120,
            output_path: "recording.wav".into(),
        }
    }
}

/// 采样格式
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleFmt {
    S16,
    S32,
    F32,
}

impl SampleFmt {
    pub fn as_str(&self) -> &'static str {
        match self {
            SampleFmt::S16 => "s16",
            SampleFmt::S32 => "s32",
            SampleFmt::F32 => "f32",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "s16" => Some(SampleFmt::S16),
            "s32" => Some(SampleFmt::S32),
            "f32" => Some(SampleFmt::F32),
            _ => None,
        }
    }

    pub fn to_hound_sample_format(&self) -> hound::SampleFormat {
        match self {
            SampleFmt::S16 | SampleFmt::S32 => hound::SampleFormat::Int,
            SampleFmt::F32 => hound::SampleFormat::Float,
        }
    }

    pub fn bits_per_sample(&self) -> u16 {
        match self {
            SampleFmt::S16 => 16,
            SampleFmt::S32 => 32,
            SampleFmt::F32 => 32,
        }
    }
}
