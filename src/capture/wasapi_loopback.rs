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
        unsafe {
            use windows::Win32::System::Com::*;
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_err() {
                eprintln!("[WASAPI] COM 初始化失败或已初始化");
            }
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            wasapi_loopback_thread(tx_clone, stop_flag_clone, init_tx, device_name)
        }));

        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("WASAPI 录制错误: {e}"),
            Err(_) => eprintln!("WASAPI 子线程 panic"),
        }
    });

    let init_result = init_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("等待初始化超时: {e}"))?;

    match init_result {
        InitResult::Success { sample_rate, sample_fmt } => {
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

    // 创建设备枚举器
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| {
        let msg = format!("创建设备枚举器失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;

    // 获取默认渲染设备
    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|e| {
            let msg = format!("获取默认扬声器设备失败: {e}");
            let _ = init_tx.send(InitResult::Failed(msg.clone()));
            msg
        })?;

    // 激活音频客户端
    let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
        let msg = format!("激活音频客户端失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;

    // 获取混音格式
    let mix_format_ptr = audio_client.GetMixFormat().map_err(|e| {
        let msg = format!("获取混音格式失败: {e}");
        let _ = init_tx.send(InitResult::Failed(msg.clone()));
        msg
    })?;
    let mix_format = &*mix_format_ptr;

    let channels = mix_format.nChannels as u16;
    let sample_rate = mix_format.nSamplesPerSec as u32;
    let bits_per_sample = mix_format.wBitsPerSample as u16;

    eprintln!("[WASAPI] 格式: {}Hz, {}ch, {}bit", sample_rate, channels, bits_per_sample);

    // 通知初始化成功
    let _ = init_tx.send(InitResult::Success {
        sample_rate,
        sample_fmt: SampleFmt::F32,
    });

    // 配置客户端
    let stream_flags = AUDCLNT_STREAMFLAGS_LOOPBACK.0 | AUDCLNT_STREAMFLAGS_NOPERSIST.0;
    let hns_buffer_duration = 10000000i64; // 1秒
    let hns_periodicity = 0i64;

    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            hns_buffer_duration,
            hns_periodicity,
            mix_format,
            None,
        )
        .map_err(|e| format!("初始化客户端失败: {e}"))?;

    // 获取缓冲区大小
    let buffer_frames = audio_client.GetBufferSize().map_err(|e| format!("获取缓冲区大小失败: {e}"))?;
    eprintln!("[WASAPI] 缓冲区: {} 帧", buffer_frames);

    // 获取 Capture 客户端
    let capture_client: IAudioCaptureClient = audio_client
        .GetService::<IAudioCaptureClient>()
        .map_err(|e| format!("获取 Capture 服务失败: {e}"))?;

    // 启动
    audio_client.Start().map_err(|e| format!("启动失败: {e}"))?;
    eprintln!("[WASAPI] 已启动");

    let start = Instant::now();
    let bytes_per_sample = (bits_per_sample / 8) as usize;

    while !stop_flag.load(Ordering::Relaxed) {
        let packet_size = match capture_client.GetNextPacketSize() {
            Ok(s) => s,
            Err(_) => break,
        };

        if packet_size == 0 {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut num_frames: u32 = 0;
        let mut flags: u32 = 0;

        if let Err(e) = capture_client.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None) {
            eprintln!("[WASAPI] GetBuffer 失败: {:?}", e);
            break;
        }

        if num_frames > 0 && !data_ptr.is_null() {
            let samples: Vec<f64> = match bits_per_sample {
                32 => {
                    let ptr = data_ptr as *const f32;
                    (0..num_frames)
                        .map(|i| ptr.add(i as usize * channels as usize).read() as f64)
                        .collect()
                }
                16 => {
                    let ptr = data_ptr as *const i16;
                    (0..num_frames)
                        .map(|i| ptr.add(i as usize * channels as usize).read() as f64 / 32768.0)
                        .collect()
                }
                _ => {
                    break;
                }
            };

            // 只取第一通道
            let mono: Vec<f64> = samples.iter().step_by(channels as usize).copied().collect();
            let _ = tx.send(mono);
        }

        let _ = capture_client.ReleaseBuffer(num_frames);
    }

    eprintln!("[WASAPI] 停止");
    let _ = audio_client.Stop();
    windows::Win32::System::Com::CoTaskMemFree(Some(mix_format_ptr as *mut std::ffi::c_void));

    Ok(())
}
