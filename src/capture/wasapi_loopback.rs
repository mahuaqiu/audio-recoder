//! Windows 扬声器录制 - 使用 WASAPI loopback 捕获系统音频

use crate::capture::{RecordConfig, SampleFmt, StopHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// 初始化结果
enum InitResult {
    Success {
        sample_rate: u32,
        sample_fmt: SampleFmt,
    },
    Failed(String),
}

/// 使用 WASAPI loopback 录制扬声器音频（仅 Windows）
pub fn record_speaker(
    _config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    // 创建通道用于传递初始化结果
    let (init_tx, init_rx) = mpsc::channel();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    let tx_clone = tx.clone();

    eprintln!("正在录制系统音频 (WASAPI loopback)...");

    thread::spawn(move || {
        eprintln!("[WASAPI] 子线程启动，开始初始化...");

        // 在子线程中初始化 COM（STA 模式，WASAPI loopback 推荐）
        unsafe {
            use windows::Win32::System::Com::*;
            eprintln!("[WASAPI] 正在初始化 COM...");
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_err() {
                eprintln!("[WASAPI] COM 初始化失败或已初始化，hr={:?}", hr);
            } else {
                eprintln!("[WASAPI] COM 初始化成功");
            }
        }

        eprintln!("[WASAPI] 准备调用 wasapi_loopback_thread...");

        // 用 catch_unwind 捕获子线程的 panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            wasapi_loopback_thread(tx_clone, stop_flag_clone, init_tx)
        }));

        // 子线程结束后释放 COM
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }

        match result {
            Ok(inner) => {
                match inner {
                    Ok(()) => {
                        // 初始化成功已在 wasapi_loopback_thread 内部通知主线程
                    }
                    Err(e) => {
                        eprintln!("WASAPI loopback 录制错误: {e}");
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
            }
        }
        eprintln!("[WASAPI] 子线程结束");
    });

    // 等待初始化完成（最多 10 秒）
    let init_result = init_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("等待 WASAPI 初始化超时: {e}"))?;

    match init_result {
        InitResult::Success {
            sample_rate,
            sample_fmt,
        } => Ok(StopHandle::new_speaker(stop_flag, sample_rate, sample_fmt)),
        InitResult::Failed(e) => Err(e),
    }
}

/// 检测样本块是否"有声音"（能量超过阈值）
fn is_silent(samples: &[f64], threshold: f64) -> bool {
    let sum: f64 = samples.iter().map(|s| s * s).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    rms < threshold
}

/// 对样本应用淡入：开头 N 个样本从 0 渐变到 1
fn fade_in(samples: &mut [f64], fade_frames: usize) {
    let n = fade_frames.min(samples.len());
    for (i, sample) in samples.iter_mut().enumerate() {
        if i < n {
            let gain = i as f64 / n as f64;
            *sample *= gain;
        }
    }
}

/// 对样本应用淡出：最后 N 个样本从 1 渐变到 0
fn fade_out(samples: &mut [f64], fade_frames: usize) {
    let n = fade_frames.min(samples.len());
    let len = samples.len();
    for (i, sample) in samples.iter_mut().enumerate() {
        if i >= len - n {
            let gain = (len - i) as f64 / n as f64;
            *sample *= gain;
        }
    }
}

unsafe fn wasapi_loopback_thread(
    tx: mpsc::Sender<Vec<f64>>,
    stop_flag: Arc<AtomicBool>,
    init_tx: mpsc::Sender<InitResult>,
) -> Result<(), String> {
    eprintln!("[WASAPI] wasapi_loopback_thread 开始执行");

    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    eprintln!("[WASAPI] 正在创建设备枚举器...");
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| {
        let msg = format!("创建设备枚举器失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    eprintln!("[WASAPI] 设备枚举器创建成功");

    eprintln!("[WASAPI] 正在获取默认渲染设备...");
    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|e| {
            let msg = format!("获取默认扬声器设备失败: {e}");
            let _ = init_tx.send(InitResult::Failed(msg.clone()));
            msg
        })?;
    eprintln!("[WASAPI] 获取默认渲染设备成功");

    eprintln!("[WASAPI] 正在激活音频客户端...");
    let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
        let msg = format!("激活音频客户端失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    eprintln!("[WASAPI] 音频客户端激活成功");

    // 获取设备混音格式
    eprintln!("[WASAPI] 正在获取混音格式...");
    let mix_format_ptr = audio_client.GetMixFormat().map_err(|e| {
        let msg = format!("获取设备混音格式失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    eprintln!("[WASAPI] 获取混音格式成功");

    let fmt = &*mix_format_ptr;
    let sample_rate = fmt.nSamplesPerSec;
    let bits_per_sample = fmt.wBitsPerSample;
    let format_tag = fmt.wFormatTag;
    let num_channels = fmt.nChannels;

    // 判断采样格式
    let sample_fmt = if format_tag == 3 {
        SampleFmt::F32
    } else if format_tag == 65534 {
        if bits_per_sample == 32 {
            SampleFmt::F32
        } else if bits_per_sample == 16 {
            SampleFmt::S16
        } else {
            SampleFmt::S32
        }
    } else if bits_per_sample == 16 {
        SampleFmt::S16
    } else {
        SampleFmt::S32
    };

    eprintln!(
        "混音格式详情: {}Hz, {}bit, tag={}, channels={}",
        sample_rate, bits_per_sample, format_tag, num_channels
    );

    // 初始化音频客户端（loopback 模式必须用混音格式）
    eprintln!("[WASAPI] 正在初始化音频客户端...");
    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000, // 1秒缓冲
            0,
            mix_format_ptr,
            None,
        )
        .map_err(|e| {
            let msg = format!("初始化音频客户端失败: {e}");
            let _ = init_tx.send(InitResult::Failed(msg.clone()));
            msg
        })?;
    eprintln!("[WASAPI] 音频客户端初始化成功");

    eprintln!("[WASAPI] 正在获取捕获客户端...");
    let capture_client: IAudioCaptureClient = audio_client.GetService().map_err(|e| {
        let msg = format!("获取捕获客户端失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    eprintln!("[WASAPI] 获取捕获客户端成功");

    eprintln!("[WASAPI] 正在启动音频捕获...");
    audio_client.Start().map_err(|e| {
        let msg = format!("启动音频捕获失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    eprintln!("[WASAPI] 音频捕获已启动");

    // 通知主线程初始化成功
    eprintln!(
        "[WASAPI] 通知主线程初始化成功: {}Hz, {}",
        sample_rate,
        sample_fmt.as_str()
    );
    let _ = init_tx.send(InitResult::Success {
        sample_rate,
        sample_fmt,
    });

    // 淡入淡出的帧数（约 5ms 的样本量，避免爆破音）
    let fade_frames = (sample_rate as usize) / 200; // 5ms
                                                    // 静音检测阈值（rms < 0.001 视为静音）
    let silence_threshold = 0.001;

    // 状态追踪
    let mut frames_written: u64 = 0;
    let mut was_silent = true; // 初始状态视为静音（等待第一段音频）
    let start_time = Instant::now();

    // 录制循环
    while !stop_flag.load(Ordering::Relaxed) {
        // 根据流逝时间计算应该有多少帧，静音期间填充
        let elapsed_secs = start_time.elapsed().as_secs_f64();
        let expected_frames = (elapsed_secs * sample_rate as f64) as u64;

        if expected_frames > frames_written {
            let silence_frames = expected_frames - frames_written;
            if silence_frames > 0 {
                let silence = vec![0.0f64; silence_frames as usize];
                let _ = tx.send(silence);
                frames_written += silence_frames;
                was_silent = true; // 填充静音后状态保持为静音
            }
        }

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
                // 解析多通道交错数据，只取第一个通道
                let mut samples: Vec<f64> = match sample_fmt {
                    SampleFmt::S16 => {
                        let ptr = data_ptr as *const i16;
                        (0..num_frames)
                            .map(|i| {
                                let v = ptr.add((i as usize) * (num_channels as usize)).read();
                                v as f64 / 32768.0
                            })
                            .collect()
                    }
                    SampleFmt::S32 => {
                        let ptr = data_ptr as *const i32;
                        (0..num_frames)
                            .map(|i| {
                                let v = ptr.add((i as usize) * (num_channels as usize)).read();
                                v as f64 / 2147483648.0
                            })
                            .collect()
                    }
                    SampleFmt::F32 => {
                        let ptr = data_ptr as *const f32;
                        (0..num_frames)
                            .map(|i| {
                                let v = ptr.add((i as usize) * (num_channels as usize)).read();
                                v as f64
                            })
                            .collect()
                    }
                };

                // 检测当前音频段是否静音
                let current_silent = is_silent(&samples, silence_threshold);

                // 状态转换时应用淡入淡出
                if was_silent && !current_silent {
                    // 静音 -> 有声音：淡入
                    fade_in(&mut samples, fade_frames);
                } else if !was_silent && current_silent {
                    // 有声音 -> 静音：淡出（本次数据本身是静音，不处理）
                } else if !was_silent && !current_silent {
                    // 持续有声音：检查开头几个样本是否接近 0，若是则淡入（防止开头有突变）
                    let first_samples = &samples[..fade_frames.min(samples.len())];
                    if first_samples.iter().all(|s| s.abs() < 0.01) {
                        fade_in(&mut samples, fade_frames);
                    }
                }

                let _ = tx.send(samples);
                frames_written += num_frames as u64;
                was_silent = current_silent;
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

    // 录制结束前，补齐最后的静音
    let elapsed_secs = start_time.elapsed().as_secs_f64();
    let expected_frames = (elapsed_secs * sample_rate as f64) as u64;
    if expected_frames > frames_written {
        let silence_frames = expected_frames - frames_written;
        let silence = vec![0.0f64; silence_frames as usize];
        let _ = tx.send(silence);
    }

    let _ = audio_client.Stop();

    // 释放混音格式内存
    CoTaskMemFree(Some(mix_format_ptr as *const _));

    Ok(())
}
