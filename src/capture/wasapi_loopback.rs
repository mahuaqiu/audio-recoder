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
    config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    let (init_tx, init_rx) = mpsc::channel();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    let tx_clone = tx.clone();
    let device_name = config.device_name.clone();

    eprintln!("正在录制系统音频 (WASAPI loopback)...");

    thread::spawn(move || {
        eprintln!("[WASAPI] 子线程启动，开始初始化...");

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

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            wasapi_loopback_thread(tx_clone, stop_flag_clone, init_tx, device_name)
        }));

        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }

        match result {
            Ok(inner) => {
                if let Err(e) = inner {
                    eprintln!("WASAPI loopback 录制错误: {e}");
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

    let init_result = init_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("等待 WASAPI 初始化超时: {e}"))?;

    match init_result {
        InitResult::Success {
            sample_rate,
            sample_fmt,
        } => {
            eprintln!("WASAPI 初始化成功，采样率: {}Hz, 格式: {}", sample_rate, sample_fmt.as_str());
            Ok(StopHandle::new_speaker(stop_flag, sample_rate, sample_fmt))
        }
        InitResult::Failed(msg) => Err(msg),
    }
}

unsafe fn wasapi_loopback_thread(
    tx: mpsc::Sender<Vec<f64>>,
    stop_flag: Arc<AtomicBool>,
    init_tx: mpsc::Sender<InitResult>,
    device_name: Option<String>,
) -> Result<(), String> {
    eprintln!("[WASAPI] wasapi_loopback_thread 开始执行");

    // 先用 cpal 验证设备名称是否存在
    if let Some(ref name) = device_name {
        use cpal::HostTrait;
        let host = cpal::default_host();
        let name_lower = name.to_lowercase();
        let devices: Vec<_> = host.output_devices()
            .map_err(|e| format!("枚举输出设备失败: {}", e))?
            .filter_map(|d| d.name().ok())
            .collect();
        
        if !devices.iter().any(|n| n.to_lowercase().contains(&name_lower)) {
            let list = devices.iter()
                .enumerate()
                .map(|(i, n)| format!("  [{}] {}", i, n))
                .collect::<Vec<_>>()
                .join("\n");
            let err = format!("未找到名称包含 \"{}\" 的输出设备\n可用设备:\n{}", name, list);
            let _ = init_tx.send(InitResult::Failed(err.clone()));
            return Err(err);
        }
        eprintln!("[WASAPI] 设备名称验证通过: {}", name);
    }

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

    // 获取默认渲染设备（设备名称已在上方验证）
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

    eprintln!("[WASAPI] 正在获取混音格式...");
    let mix_format_ptr = audio_client.GetMixFormat().map_err(|e| {
        let msg = format!("获取设备混音格式失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    let mix_format = &*mix_format_ptr;
    eprintln!("[WASAPI] 混音格式: {:?}", mix_format);

    // 解析格式
    let channels = mix_format.nChannels as u16;
    let sample_rate = mix_format.nSamplesPerSec;
    let bits_per_sample = mix_format.wBitsPerSample as u16;
    let block_align = mix_format.nBlockAlign as u16;

    eprintln!(
        "[WASAPI] 格式: {}Hz, {}通道, {}bit, block_align={}",
        sample_rate, channels, bits_per_sample, block_align
    );

    // 确定输出格式
    let (target_sample_rate, target_sample_fmt) = (sample_rate as u32, SampleFmt::F32);

    // 通知初始化成功
    let _ = init_tx.send(InitResult::Success {
        sample_rate: target_sample_rate,
        sample_fmt: target_sample_fmt,
    });

    // 配置客户端
    let mut stream_flags = AUDCLNT_STREAMFLAGS_NOPERSIST;
    stream_flags.0 |= AUDCLNT_STREAMFLAGS_LOOPBACK.0;

    let hns_buffer_duration = 10000000i64; // 1秒缓冲区
    let hns_periodicity = 0i64;

    eprintln!("[WASAPI] 正在初始化客户端...");
    audio_client
        .Initialize(
            AUDCLNT_SESSIONFLAGS_EXCLUSIVE,
            stream_flags,
            hns_buffer_duration,
            hns_periodicity,
            mix_format,
            None,
        )
        .map_err(|e| format!("初始化音频客户端失败: {e}"))?;
    eprintln!("[WASAPI] 客户端初始化成功");

    // 获取缓冲区大小
    let mut frame_padding = 0u32;
    let mut buffer_frames = 0u32;
    audio_client
        .GetCurrentPadding(&mut frame_padding)
        .map_err(|e| format!("获取padding失败: {e}"))?;
    buffer_frames = audio_client.GetBufferSize().map_err(|e| format!("获取缓冲区大小失败: {e}"))?;
    eprintln!("[WASAPI] 缓冲区大小: {} 帧, padding: {}", buffer_frames, frame_padding);

    // 获取 Capture 客户端
    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("获取 Capture 服务失败: {e}"))?;
    eprintln!("[WASAPI] 获取 Capture 服务成功");

    // 启动客户端
    audio_client.Start().map_err(|e| format!("启动客户端失败: {e}"))?;
    eprintln!("[WASAPI] 客户端已启动");

    // 录音循环
    let start = Instant::now();
    let bytes_per_sample = (bits_per_sample / 8) as usize;

    while !stop_flag.load(Ordering::Relaxed) {
        // 获取可用的帧数
        let mut padding = 0u32;
        if let Err(e) = audio_client.GetCurrentPadding(&mut padding) {
            eprintln!("[WASAPI] 获取 padding 失败: {}", e);
            break;
        }

        let frames_available = if padding > 0 { padding } else { continue };

        // 获取数据
        let mut data = std::ptr::null_mut();
        let mut flags = 0u32;
        let mut num_frames_to_read = frames_available;
        let hr = capture_client.GetBuffer(&mut data, &mut flags, &mut num_frames_to_read, std::ptr::null_mut());

        if hr.is_err() {
            let err_code = hr.unwrap_err();
            if err_code.0 as u32 == 0x88890018 {
                // AUDCLNT_E_BUFFER_EMPTY
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            eprintln!("[WASAPI] GetBuffer 失败: {:?}", err_code);
            break;
        }

        if num_frames_to_read > 0 {
            let data: *mut u8 = data as *mut u8;
            let sample_count = num_frames_to_read as usize * channels as usize;
            let byte_count = sample_count * bytes_per_sample;

            // 转换为 f64
            let samples: Vec<f64> = match bits_per_sample {
                32 => {
                    let f32_data = std::slice::from_raw_parts(data as *const f32, sample_count);
                    f32_data.iter().map(|&s| s as f64).collect()
                }
                16 => {
                    let i16_data = std::slice::from_raw_parts(data as *const i16, sample_count);
                    i16_data.iter().map(|&s| s as f64 / 32768.0).collect()
                }
                24 => {
                    // 24位需要转换
                    let mut result = Vec::with_capacity(sample_count);
                    for i in 0..num_frames_to_read as usize {
                        for ch in 0..channels as usize {
                            let offset = (i * channels as usize + ch as usize) * 3;
                            let b0 = data.offset(offset as isize) as u32;
                            let b1 = data.offset(offset as isize + 1) as u32;
                            let b2 = data.offset(offset as isize + 2) as u32;
                            let sample = ((b2 << 16) | (b1 << 8) | b0) as i32;
                            let sample = if sample & 0x800000 != 0 {
                                sample | !0xFFFFFF
                            } else {
                                sample
                            };
                            result.push(sample as f64 / 8388608.0);
                        }
                    }
                    result
                }
                _ => {
                    eprintln!("[WASAPI] 不支持的位深: {}", bits_per_sample);
                    break;
                }
            };

            // 只取第一个通道
            let mono_samples: Vec<f64> = samples.iter().step_by(channels as usize).copied().collect();
            let _ = tx.send(mono_samples);
        }

        let _ = capture_client.ReleaseBuffer(num_frames_to_read);

        std::thread::sleep(Duration::from_millis(1));
    }

    eprintln!("[WASAPI] 停止录音");
    let _ = audio_client.Stop();

    // 释放混音格式
    windows::Win32::System::Com::CoTaskMemFree(Some(mix_format_ptr as *mut std::ffi::c_void));

    Ok(())
}
