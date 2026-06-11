//! Windows 扬声器录制 - 使用 WASAPI loopback 捕获系统音频

use crate::capture::{RecordConfig, StopHandle, InitStatus, SampleFmt};
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
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            // COM 可能已经初始化过了
        }
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    let tx_clone = tx.clone();

    // 先查询设备的混音格式，用实际格式打开
    let (actual_rate, actual_fmt, mix_format) = unsafe {
        query_device_mix_format()?
    };

    eprintln!("正在录制系统音频 (WASAPI loopback)...");
    eprintln!("设备混音格式: {}Hz, {}", actual_rate, actual_fmt.as_str());

    thread::spawn(move || {
        let result = unsafe {
            wasapi_loopback_thread(actual_rate, actual_fmt, mix_format, tx_clone, stop_flag_clone)
        };
        
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

    let handle = StopHandle::new_speaker_with_status(stop_flag, init_rx, actual_rate, actual_fmt);
    
    Ok(handle)
}

/// 查询默认音频渲染设备的混音格式
unsafe fn query_device_mix_format() -> Result<(u32, SampleFmt, WAVEFORMATEX), String> {
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

    let mix_format_ptr = audio_client
        .GetMixFormat()
        .map_err(|e| format!("获取设备混音格式失败: {e}"))?;

    let fmt = &*mix_format_ptr;
    let sample_rate = fmt.nSamplesPerSec;
    let bits_per_sample = fmt.wBitsPerSample;
    let format_tag = fmt.wFormatTag;

    let sample_fmt = if format_tag == 3 {
        SampleFmt::F32
    } else if bits_per_sample == 16 {
        SampleFmt::S16
    } else {
        SampleFmt::S32
    };

    eprintln!("混音格式详情: {}Hz, {}bit, tag={}, channels={}",
        sample_rate, bits_per_sample, format_tag, fmt.nChannels);

    // 复制格式数据
    let mix_format = *mix_format_ptr;
    
    // 释放 WASAPI 返回的内存
    CoTaskMemFree(Some(mix_format_ptr as *const _));

    Ok((sample_rate, sample_fmt, mix_format))
}

unsafe fn wasapi_loopback_thread(
    actual_rate: u32,
    actual_fmt: SampleFmt,
    mix_format: WAVEFORMATEX,
    tx: mpsc::Sender<Vec<f64>>,
    stop_flag: Arc<AtomicBool>,
) -> Result<(), String> {
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

    // 使用设备混音格式初始化（loopback 模式必须用混音格式）
    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000, // 1秒缓冲
            0,
            &mix_format,
            None,
        )
        .map_err(|e| format!("初始化音频客户端失败: {e}"))?;

    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("获取捕获客户端失败: {e}"))?;

    let num_channels = mix_format.nChannels;

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
                let samples = match actual_fmt {
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

    Ok(())
}
