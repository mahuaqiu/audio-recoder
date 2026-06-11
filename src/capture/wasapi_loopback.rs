//! Windows 扬声器录制 - 使用 WASAPI loopback 捕获系统音频

use crate::capture::{RecordConfig, StopHandle, InitStatus};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 使用 WASAPI loopback 录制扬声器音频（仅 Windows）
pub fn record_speaker(
    config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    use windows::Win32::System::Com::*;

    // 创建通道用于传递初始化状态
    let (init_tx, init_rx) = mpsc::channel();

    unsafe {
        // 初始化 COM（每个线程需要单独初始化）
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            // COM 可能已经初始化过了，忽略错误
        }
    }

    // 创建停止标志
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    let sample_rate = config.sample_rate;
    let sample_fmt = config.sample_fmt;
    let tx_clone = tx.clone();

    // 在新线程中运行 WASAPI 录制
    thread::spawn(move || {
        let result = unsafe {
            wasapi_loopback_thread(sample_rate, sample_fmt, tx_clone, stop_flag_clone)
        };
        
        // 报告初始化/运行结果
        match result {
            Ok(_) => {
                let _ = init_tx.send(InitStatus::Success);
            }
            Err(e) => {
                eprintln!("WASAPI loopback 录制错误: {e}");
                let _ = init_tx.send(InitStatus::Failed);
            }
        }
    });

    // 等待初始化结果
    eprintln!("正在录制系统音频 (WASAPI loopback)...");
    
    // 返回带状态的 StopHandle
    let handle = StopHandle::new_speaker_with_status(stop_flag, init_rx);
    
    Ok(handle)
}

unsafe fn wasapi_loopback_thread(
    sample_rate: u32,
    sample_fmt: crate::capture::SampleFmt,
    tx: mpsc::Sender<Vec<f64>>,
    stop_flag: Arc<AtomicBool>,
) -> Result<(), String> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    // 获取默认音频渲染���备（扬声器）
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("创建设备枚举器失败: {e}"))?;

    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|e| format!("获取默认扬声器设备失败: {e}"))?;

    // 激活 IAudioClient
    let audio_client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("激活音频客户端失败: {e}"))?;

    // 设置 loopback 模式格式
    let wave_format = match sample_fmt {
        crate::capture::SampleFmt::S16 => WAVEFORMATEX {
            wFormatTag: 1 as _, // PCM
            nChannels: 1,
            nSamplesPerSec: sample_rate,
            wBitsPerSample: 16,
            nBlockAlign: 2,
            nAvgBytesPerSec: sample_rate * 2,
            cbSize: 0,
        },
        crate::capture::SampleFmt::S32 => WAVEFORMATEX {
            wFormatTag: 1 as _,
            nChannels: 1,
            nSamplesPerSec: sample_rate,
            wBitsPerSample: 32,
            nBlockAlign: 4,
            nAvgBytesPerSec: sample_rate * 4,
            cbSize: 0,
        },
        crate::capture::SampleFmt::F32 => WAVEFORMATEX {
            wFormatTag: 3 as _, // IEEE_FLOAT
            nChannels: 1,
            nSamplesPerSec: sample_rate,
            wBitsPerSample: 32,
            nBlockAlign: 4,
            nAvgBytesPerSec: sample_rate * 4,
            cbSize: 0,
        },
    };

    // 以 loopback 模式初始化（AUDCLNT_STREAMFLAGS_LOOPBACK = 0x00020000）
    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000, // 1秒缓冲（100ns 单位）
            0,
            &wave_format,
            None,
        )
        .map_err(|e| format!("初始化音频客户端失败: {e}"))?;

    // 获取捕获客户端
    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("获取捕获客户端失败: {e}"))?;

    // 开始录制
    audio_client
        .Start()
        .map_err(|e| format!("启动音频捕获失败: {e}"))?;

    // 录制循环
    while !stop_flag.load(Ordering::Relaxed) {
        unsafe {
            let mut packet_size = capture_client
                .GetNextPacketSize()
                .map_err(|e| format!("获取数据包大小失败: {e}"))?;

            while packet_size > 0 {
                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                capture_client
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
                    .map_err(|e| format!("获取缓冲区失败: {e}"))?;

                if num_frames > 0 && !data_ptr.is_null() {
                    let samples = match sample_fmt {
                        crate::capture::SampleFmt::S16 => {
                            let ptr = data_ptr as *const i16;
                            (0..num_frames).map(|i| ptr.add(i as usize).read() as f64).collect()
                        }
                        crate::capture::SampleFmt::S32 => {
                            let ptr = data_ptr as *const i32;
                            (0..num_frames).map(|i| ptr.add(i as usize).read() as f64).collect()
                        }
                        crate::capture::SampleFmt::F32 => {
                            let ptr = data_ptr as *const f32;
                            (0..num_frames).map(|i| ptr.add(i as usize).read() as f64).collect()
                        }
                    };

                    let _ = tx.send(samples);
                }

                capture_client
                    .ReleaseBuffer(num_frames)
                    .map_err(|e| format!("释放缓冲区失败: {e}"))?;

                packet_size = capture_client
                    .GetNextPacketSize()
                    .map_err(|e| format!("获取数据包大小失败: {e}"))?;
            }
        }

        // 短暂休眠避免 CPU 空转
        std::thread::sleep(Duration::from_millis(10));
    }

    // 停止录制
    let _ = audio_client.Stop();

    Ok(())
}
