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
    device_name: Option<String>,
) -> Result<(), String> {
    // 先用 cpal 验证设备名称是否存在
    if let Some(ref name) = device_name {
        use cpal::traits::HostTrait;
        use cpal::traits::DeviceTrait;
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
    let format_tag = mix_format.wFormatTag;

    // 根据 format tag 和 bits_per_sample 判断实际采样格式
    // format_tag == 3 => IEEE_FLOAT, 65534 => EXTENSIBLE (需看 bit depth)
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

    eprintln!("[WASAPI] 格式: {}Hz, {}ch, {}bit, tag={}, fmt={}",
        sample_rate, channels, bits_per_sample, format_tag, sample_fmt.as_str());

    // 通知初始化成功
    let _ = init_tx.send(InitResult::Success {
        sample_rate,
        sample_fmt,
    });

    // 配置客户端
    let stream_flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_NOPERSIST;
    let hns_buffer_duration = 10000000i64; // 1秒
    let hns_periodicity = 0i64;

    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            hns_buffer_duration,
            hns_periodicity,
            mix_format_ptr,
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

    // 淡入淡出帧数（约 10ms）
    let fade_frames = (sample_rate as usize) / 100;
    let silence_threshold = 0.001;

    let mut frames_written: u64 = 0;
    let mut was_silent = true;
    let start_time = Instant::now();

    while !stop_flag.load(Ordering::Relaxed) {
        // 计算当前应该写入了多少帧（基于经过的时间）
        let elapsed_secs = start_time.elapsed().as_secs_f64();
        let expected_frames = (elapsed_secs * sample_rate as f64) as u64;

        // 如果时间轴落后，补齐静音帧
        if expected_frames > frames_written {
            let silence_frames = expected_frames - frames_written;
            if silence_frames > 0 {
                let silence = vec![0.0f64; silence_frames as usize];
                let _ = tx.send(silence);
                frames_written += silence_frames;
                was_silent = true;
            }
        }

        // 读取所有可用的数据包
        let mut packet_size = match capture_client.GetNextPacketSize() {
            Ok(s) => s,
            Err(_) => break,
        };

        while packet_size > 0 {
            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames: u32 = 0;
            let mut flags: u32 = 0;

            if let Err(e) = capture_client.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None) {
                eprintln!("[WASAPI] GetBuffer 失败: {:?}", e);
                break;
            }

            if num_frames > 0 && !data_ptr.is_null() {
                // 根据 sample_fmt 读取数据，只取第一通道
                let mut samples: Vec<f64> = match sample_fmt {
                    SampleFmt::F32 => {
                        let ptr = data_ptr as *const f32;
                        (0..num_frames)
                            .map(|i| ptr.add(i as usize * channels as usize).read() as f64)
                            .collect()
                    }
                    SampleFmt::S16 => {
                        let ptr = data_ptr as *const i16;
                        (0..num_frames)
                            .map(|i| ptr.add(i as usize * channels as usize).read() as f64 / 32768.0)
                            .collect()
                    }
                    SampleFmt::S32 => {
                        let ptr = data_ptr as *const i32;
                        (0..num_frames)
                            .map(|i| ptr.add(i as usize * channels as usize).read() as f64 / 2147483648.0)
                            .collect()
                    }
                };

                let current_silent = is_silent(&samples, silence_threshold);

                // 静音 → 有声音：淡入
                if was_silent && !current_silent {
                    fade_in(&mut samples, fade_frames);
                }
                // 有声音 → 静音：淡出
                else if !was_silent && current_silent {
                    fade_out(&mut samples, fade_frames);
                }

                let _ = tx.send(samples);
                frames_written += num_frames as u64;
                was_silent = current_silent;
            }

            let _ = capture_client.ReleaseBuffer(num_frames);

            packet_size = match capture_client.GetNextPacketSize() {
                Ok(s) => s,
                Err(_) => break,
            };
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // 录制结束前补齐静音帧到当前时间
    let elapsed_secs = start_time.elapsed().as_secs_f64();
    let expected_frames = (elapsed_secs * sample_rate as f64) as u64;
    if expected_frames > frames_written {
        let silence_frames = expected_frames - frames_written;
        let silence = vec![0.0f64; silence_frames as usize];
        let _ = tx.send(silence);
    }

    eprintln!("[WASAPI] 停止");
    let _ = audio_client.Stop();
    windows::Win32::System::Com::CoTaskMemFree(Some(mix_format_ptr as *mut std::ffi::c_void));

    Ok(())
}
