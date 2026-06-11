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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// 录制状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordStatus {
    /// 初始化中
    Initializing,
    /// 正在录制
    Recording,
    /// 已停止
    Stopped,
    /// 初始化/录制失败
    Failed,
}

/// 扬声器初始化状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitStatus {
    Success,
    Failed,
}

/// 录制停止句柄，drop 时停止录制
pub struct StopHandle {
    /// cpal 音频流，用于麦克风录制
    stream: Option<cpal::Stream>,
    /// 停止标志，用于扬声器录制
    stop_flag: Option<Arc<AtomicBool>>,
    /// 录制状态
    status: Arc<AtomicU8>,
    /// 初始化结果接收器（仅用于扬声器）
    init_rx: Option<mpsc::Receiver<InitStatus>>,
}

impl StopHandle {
    /// 创建麦克风录制的停止句柄
    pub fn new_microphone(stream: cpal::Stream) -> Self {
        Self {
            stream: Some(stream),
            stop_flag: None,
            status: Arc::new(AtomicU8::new(RecordStatus::Recording as u8)),
            init_rx: None,
        }
    }

    /// 创建扬声器录制的停止句柄（带初始化状态接收器）
    pub fn new_speaker_with_status(
        stop_flag: Arc<AtomicBool>,
        init_rx: mpsc::Receiver<InitStatus>,
    ) -> Self {
        Self {
            stream: None,
            stop_flag: Some(stop_flag),
            status: Arc::new(AtomicU8::new(RecordStatus::Initializing as u8)),
            init_rx: Some(init_rx),
        }
    }

    /// 检查录制是否正在运行（初始化成功且未停止）
    pub fn is_recording(&self) -> bool {
        // 如果有 init_rx，先检查初始化状态
        if let Some(rx) = &self.init_rx {
            // 非阻塞尝试接收
            if let Ok(status) = rx.try_recv() {
                match status {
                    InitStatus::Success => {
                        self.status.store(RecordStatus::Recording as u8, Ordering::Relaxed);
                    }
                    InitStatus::Failed => {
                        self.status.store(RecordStatus::Failed as u8, Ordering::Relaxed);
                        return false;
                    }
                }
            } else if let Ok(status) = rx.recv_timeout(Duration::from_millis(10)) {
                // 超时后再次尝试接收
                match status {
                    InitStatus::Success => {
                        self.status.store(RecordStatus::Recording as u8, Ordering::Relaxed);
                    }
                    InitStatus::Failed => {
                        self.status.store(RecordStatus::Failed as u8, Ordering::Relaxed);
                        return false;
                    }
                }
            }
        }

        // 检查状态
        let s = self.status.load(Ordering::Relaxed);
        s == RecordStatus::Recording as u8
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        // 1. 如果有 cpal::Stream，drop 会自动停止音频流（麦克风）
        if self.stream.is_some() {
            self.stream = None; // 这会 drop stream
        }
        
        // 2. 如果有停止标志，设置它来通知录制线程（扬声器）
        if let Some(flag) = self.stop_flag.take() {
            flag.store(true, Ordering::Relaxed);
        }

        // 更新状态
        self.status.store(RecordStatus::Stopped as u8, Ordering::Relaxed);
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
    /// 设备索引（从 0 开始），None 表示使用默认设备
    pub device_index: Option<usize>,
    /// 是否前台阻塞模式，false 则后台运行（默���）
    pub foreground: bool,
}

impl Default for RecordConfig {
    fn default() -> Self {
        RecordConfig {
            source: Source::Microphone,
            sample_rate: 16000,
            sample_fmt: SampleFmt::S16,
            duration_secs: 120,
            output_path: "recording.wav".into(),
            device_index: None,
            foreground: false,  // 默认后台模式
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
