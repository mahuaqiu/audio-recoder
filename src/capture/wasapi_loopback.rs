//! Windows 扬声器录制 - 使用 WASAPI loopback 捕获系统音频

use crate::capture::{StopHandle, SampleFmt, RecordConfig};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 初始化结果
enum InitResult {
    Success { sample_rate: u32, sample_fmt: SampleFmt },
    Failed(String),
}

/// 使用 WASAPI loopback 录制扬声器音频（仅 Windows）
pub fn record_speaker(
    _config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    use windows::Win32::System::Com::*;

    // 创建通道用于传递初始化结果
    let (init_tx, init_rx) = mpsc::channel();

    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            // COM 可能已经初始化过了
        }
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    let tx_clone = tx.clone();

    eprintln!("正在录制系统音频 (WASAPI loopback)...");

    thread::spawn(move || {
        // 用 catch_unwind 捕获子线程的 panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            unsafe { wasapi_loopback_thread(tx_clone, stop_flag_clone) }
        }));
        
        match result {
            Ok(inner) => {
                match inner {
                    Ok((rate, fmt)) => {
                        eprintln!("WASAPI loopback 录制已启动: {}Hz, {}", rate, fmt.as_str());
                        let _ = init_tx.send(InitResult::Success { sample_rate: rate, sample_fmt: fmt });
                    }
                    Err(e) => {
                        eprintln!("WASAPI loopback 录制错误: {e}");
                        let _ = init_tx.send(InitResult::Failed(e));
                    }
                }
            }
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic".to_string()
                };
                eprintln!("WASAPI loopback 子线程 panic: {msg}");
                let _ = init_tx.send(InitResult::Failed(format!("子线程 panic: {}", msg)));
            }
        }
    });

    // 等待初始化完成（最多 5 秒）
    let init_result = init_rx.recv_timeout(Duration::from_secs(5))
        .map_err(|e| format!("等待 WASAPI 初始化超时: {e}"))?;

    match init_result {
        InitResult::Success { sample_rate, sample_fmt } => {
            Ok(StopHandle::new_speaker(stop_flag, sample_rate, sample_fmt))
        }
        InitResult::Failed(e) => Err(e),
    }
}

unsafe fn wasapi_loopback_thread(
    tx: mpsc::Sender<Vec<f64>>,
    stop_flag: Arc<AtomicBool>,
) -> Result<(u32, SampleFmt), String> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("创建设备枚举器失败: {e}"))?;

    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|e| format!("获取默认扬声器设备失败: {e}"))?;

    let audio_client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("激活音频客户端失败: {e}"))?;

    // 获取设备混音格式
    let mix_format_ptr = audio_client
        .GetMixFormat()
        .map_err(|e| format!("获取设备混音格式失败: {e}"))?;

    let fmt = &*mix_format_ptr;
    let sample_rate = fmt.nSamplesPerSec;
    let bits_per_sample = fmt.wBitsPerSample;
    let format_tag = fmt.wFormatTag;
    let num_channels = fmt.nChannels;

    // WAVE_FORMAT_EXTENSIBLE (0xFFFE = 65534) 是 Windows 音频引擎的标准格式
    // 需要根据 bits_per_sample 判断实际格式
    let sample_fmt = if format_tag == 3 || (format_tag == 65534 && bits_per_sample == 32) {
        // IEEE_FLOAT 或 EXTENSIBLE 32-bit → f32
        SampleFmt::F32
    } else if bits_per_sample == 16 {
        SampleFmt::S16
    } else {
        SampleFmt::S32
    };

    eprintln!("混音格式详情: {}Hz, {}bit, tag={}, channels={}",
        sample_rate, bits_per_sample, format_tag, num_channels);

    // 使用设备混音格式初始化（loopback 模式必须用混音格式）
    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000, // 1秒缓冲
            0,
            mix_format_ptr,
            None,
        )
        .map_err(|e| format!("初始化音频客户端失败: {e}"))?;

    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("获取捕获客户端失败: {e}"))?;

    audio_client
        .Start()
        .map_err(|e| format!("启动音频捕获失败: {e}"))?;

    while !stop_flag.load(Ordering::Relaxed) {
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
                // WASAPI 数据是多通道交错的，只取第一个通道
                let samples = match sample_fmt {
                    SampleFmt::S16 => {
                        let ptr = data_ptr as *const i16;
                        (0..num_frames)
                            .map(|i| ptr.add((i as usize) * (num_channels as usize)).read() as f64)
                            .collect()
                    }
                    SampleFmt::S32 => {
                        let ptr = data_ptr as *const i32;
                        (0..num_frames)
                            .map(|i| ptr.add((i as usize) * (num_channels as usize)).read() as f64)
                            .collect()
                    }
                    SampleFmt::F32 => {
                        let ptr = data_ptr as *const f32;
                        (0..num_frames)
                            .map(|i| ptr.add((i as usize) * (num_channels as usize)).read() as f64)
                            .collect()
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

        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = audio_client.Stop();

    // 释放混音格式内存
    CoTaskMemFree(Some(mix_format_ptr as *const _));

    Ok((sample_rate, sample_fmt))
}
