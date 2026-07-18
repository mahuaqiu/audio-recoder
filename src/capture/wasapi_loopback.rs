//! Windows WASAPI loopback 扬声器录音。

use crate::capture::{CapturedPacket, RecordConfig, SampleFmt, StopHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

enum InitResult {
    Success {
        sample_rate: u32,
        sample_fmt: SampleFmt,
    },
    Failed(String),
}

pub fn record_speaker(
    config: &RecordConfig,
    tx: mpsc::Sender<CapturedPacket>,
) -> Result<StopHandle, String> {
    let (init_tx, init_rx) = mpsc::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop = stop_flag.clone();
    let thread_tx = tx.clone();
    let device_name = config.device_name.clone();

    eprintln!("正在录制系统音频 (WASAPI loopback)...");
    thread::spawn(move || {
        unsafe {
            use windows::Win32::System::Com::*;
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            wasapi_loopback_thread(thread_tx, thread_stop, init_tx, device_name)
        }));
        unsafe { windows::Win32::System::Com::CoUninitialize() };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("WASAPI 录音错误: {error}"),
            Err(_) => eprintln!("WASAPI 录音线程异常退出"),
        }
    });

    match init_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| format!("等待 WASAPI 初始化超时: {error}"))?
    {
        InitResult::Success {
            sample_rate,
            sample_fmt,
        } => {
            eprintln!(
                "WASAPI 初始化成功，采样率: {sample_rate}Hz，格式: {}",
                sample_fmt.as_str()
            );
            Ok(StopHandle::new_speaker(stop_flag, sample_rate, sample_fmt))
        }
        InitResult::Failed(error) => Err(error),
    }
}

fn is_silent(samples: &[f64], threshold: f64) -> bool {
    if samples.is_empty() {
        return true;
    }
    let rms =
        (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt();
    rms < threshold
}

fn fade_in(samples: &mut [f64], fade_frames: usize) {
    let count = fade_frames.min(samples.len());
    for (index, sample) in samples.iter_mut().take(count).enumerate() {
        *sample *= index as f64 / count.max(1) as f64;
    }
}

fn fade_out(samples: &mut [f64], fade_frames: usize) {
    let count = fade_frames.min(samples.len());
    let start = samples.len().saturating_sub(count);
    let length = samples.len();
    for (index, sample) in samples.iter_mut().enumerate().skip(start) {
        *sample *= (length - index) as f64 / count.max(1) as f64;
    }
}

unsafe fn wasapi_loopback_thread(
    tx: mpsc::Sender<CapturedPacket>,
    stop_flag: Arc<AtomicBool>,
    init_tx: mpsc::Sender<InitResult>,
    device_name: Option<String>,
) -> Result<(), String> {
    if let Some(name) = &device_name {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let name_lower = name.to_lowercase();
        let devices: Vec<String> = host
            .output_devices()
            .map_err(|error| format!("枚举输出设备失败: {error}"))?
            .filter_map(|device| device.name().ok())
            .collect();
        if !devices
            .iter()
            .any(|device| device.to_lowercase().contains(&name_lower))
        {
            let list = devices
                .iter()
                .enumerate()
                .map(|(index, device)| format!("  [{index}] {device}"))
                .collect::<Vec<_>>()
                .join("\n");
            let error = format!("未找到输出设备 {name:?}\n可用设备:\n{list}");
            let _ = init_tx.send(InitResult::Failed(error.clone()));
            return Err(error);
        }
    }

    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| format!("创建设备枚举器失败: {error}"))?;
    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|error| format!("获取默认扬声器失败: {error}"))?;
    let audio_client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|error| format!("激活音频客户端失败: {error}"))?;
    let mix_format_ptr = audio_client
        .GetMixFormat()
        .map_err(|error| format!("获取混音格式失败: {error}"))?;
    let mix_format = &*mix_format_ptr;
    let channels = mix_format.nChannels as u16;
    let sample_rate = mix_format.nSamplesPerSec as u32;
    let bits_per_sample = mix_format.wBitsPerSample as u16;
    let sample_fmt = if mix_format.wFormatTag == 3 {
        SampleFmt::F32
    } else if mix_format.wFormatTag == 65534 && bits_per_sample == 32 {
        SampleFmt::F32
    } else if bits_per_sample == 16 {
        SampleFmt::S16
    } else {
        SampleFmt::S32
    };
    eprintln!(
        "[WASAPI] 格式: {sample_rate}Hz, {channels}ch, {bits_per_sample}bit, {}",
        sample_fmt.as_str()
    );
    let _ = init_tx.send(InitResult::Success {
        sample_rate,
        sample_fmt,
    });

    audio_client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_NOPERSIST,
            10_000_000,
            0,
            mix_format_ptr,
            None,
        )
        .map_err(|error| format!("初始化 WASAPI 客户端失败: {error}"))?;
    let buffer_frames = audio_client
        .GetBufferSize()
        .map_err(|error| format!("获取缓冲区大小失败: {error}"))?;
    eprintln!("[WASAPI] 缓冲区: {buffer_frames} 帧");
    let capture_client: IAudioCaptureClient = audio_client
        .GetService::<IAudioCaptureClient>()
        .map_err(|error| format!("获取捕获服务失败: {error}"))?;
    audio_client
        .Start()
        .map_err(|error| format!("启动 WASAPI 失败: {error}"))?;

    let fade_frames = (sample_rate as usize / 100).max(1);
    let mut was_silent = true;
    while !stop_flag.load(Ordering::Relaxed) {
        let mut packet_size = capture_client
            .GetNextPacketSize()
            .map_err(|error| format!("获取 WASAPI packet 大小失败: {error}"))?;
        while packet_size > 0 {
            let mut data_ptr = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            let mut device_position = 0u64;
            let mut qpc_position = 0u64;
            capture_client
                .GetBuffer(
                    &mut data_ptr,
                    &mut num_frames,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                )
                .map_err(|error| format!("GetBuffer 失败: {error}"))?;

            let mut samples =
                if (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data_ptr.is_null() {
                    vec![0.0; num_frames as usize]
                } else {
                    match sample_fmt {
                        SampleFmt::F32 => {
                            let pointer = data_ptr as *const f32;
                            (0..num_frames as usize)
                                .map(|index| pointer.add(index * channels as usize).read() as f64)
                                .collect()
                        }
                        SampleFmt::S16 => {
                            let pointer = data_ptr as *const i16;
                            (0..num_frames as usize)
                                .map(|index| {
                                    pointer.add(index * channels as usize).read() as f64 / 32768.0
                                })
                                .collect()
                        }
                        SampleFmt::S32 => {
                            let pointer = data_ptr as *const i32;
                            (0..num_frames as usize)
                                .map(|index| {
                                    pointer.add(index * channels as usize).read() as f64
                                        / 2_147_483_648.0
                                })
                                .collect()
                        }
                    }
                };
            let current_silent = is_silent(&samples, 0.001);
            if was_silent && !current_silent {
                fade_in(&mut samples, fade_frames);
            } else if !was_silent && current_silent {
                fade_out(&mut samples, fade_frames);
            }
            let _ = tx.send(CapturedPacket {
                samples,
                device_position: Some(device_position),
                qpc_100ns: Some(qpc_position),
                flags,
            });
            was_silent = current_silent;
            capture_client
                .ReleaseBuffer(num_frames)
                .map_err(|error| format!("ReleaseBuffer 失败: {error}"))?;
            packet_size = capture_client
                .GetNextPacketSize()
                .map_err(|error| format!("获取后续 packet 大小失败: {error}"))?;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = audio_client.Stop();
    CoTaskMemFree(Some(mix_format_ptr as *mut std::ffi::c_void));
    Ok(())
}
