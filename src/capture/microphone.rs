use crate::capture::StopHandle;
use crate::capture::{RecordConfig, SampleFmt};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::sync::mpsc;

/// 使用 cpal 录制麦克风音频
/// 采样数据通过 tx 发送，返回停止句柄
pub fn record_microphone(
    config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    let host = cpal::default_host();

    // 选择设备：如果指定了设备索引则使用，否则使用默认设备
    let device = if let Some(idx) = config.device_index {
        host.input_devices()
            .map_err(|e| format!("枚举输入设备失败: {e}"))?
            .nth(idx)
            .ok_or_else(|| format!("未找到索引为 {} 的输入设备", idx))?
    } else {
        host.default_input_device().ok_or("未找到麦克风设备")?
    };

    eprintln!("麦克风设备: {}", device.name().unwrap_or_default());

    // 收集所有支持的配置范围
    let supported_config_ranges: Vec<_> = device
        .supported_input_configs()
        .map_err(|e| format!("查询设备配置失败: {e}"))?
        .collect();

    if supported_config_ranges.is_empty() {
        return Err("设备不支持任何音频配置".to_string());
    }

    // 尝试找到匹配的配置，先精确匹配，然后 fallback
    let (selected_range, used_sample_rate, used_sample_fmt) = find_best_config(
        &supported_config_ranges,
        config.sample_rate,
        config.sample_fmt,
    )?;

    // 如果实际使用的参数与请求的不同，打印提示
    if used_sample_rate != config.sample_rate || used_sample_fmt != config.sample_fmt {
        eprintln!(
            "提示: 设备不支持请求的参数，已自动适配 - 采样率: {}Hz, 格式: {}",
            used_sample_rate,
            used_sample_fmt.as_str()
        );
    }

    // 使用选中的配置范围，设置采样率
    let stream_config = selected_range
        .with_sample_rate(cpal::SampleRate(used_sample_rate))
        .config();

    let num_channels = stream_config.channels as usize;

    let err_fn = |err: cpal::StreamError| {
        eprintln!("音频流错误: {err}");
    };

    // 根据实际支持的采样格式构建对应的流
    let stream = match used_sample_fmt {
        SampleFmt::F32 => device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // 只取第一个通道的数据
                    let samples: Vec<f64> = data
                        .iter()
                        .step_by(num_channels)
                        .map(|&s| s as f64)
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("创建音频流失败: {e}"))?,
        SampleFmt::S16 => device
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    // S16 是 16 位整数，范围 -32768~32767，需要归一化到 -1.0~1.0
                    let samples: Vec<f64> = data
                        .iter()
                        .step_by(num_channels)
                        .map(|&s| s as f64 / 32768.0)
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("创建音频流失败: {e}"))?,
        SampleFmt::S32 => device
            .build_input_stream(
                &stream_config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    // S32 是 32 位整数，范围 -2147483648~2147483647，需要归一化
                    let samples: Vec<f64> = data
                        .iter()
                        .step_by(num_channels)
                        .map(|&s| s as f64 / 2147483648.0)
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("创建音频流失败: {e}"))?,
    };

    stream.play().map_err(|e| format!("启动音频流失败: {e}"))?;

    Ok(StopHandle::new_microphone(
        stream,
        used_sample_rate,
        used_sample_fmt,
    ))
}

/// 从支持的配置范围中找到最佳匹配，优先精确匹配，然后 fallback
fn find_best_config(
    ranges: &[cpal::SupportedStreamConfigRange],
    requested_rate: u32,
    requested_fmt: SampleFmt,
) -> Result<(cpal::SupportedStreamConfigRange, u32, SampleFmt), String> {
    // 1. 首先尝试精确匹配（采样率 + 格式）
    if let Some(range) = ranges.iter().find(|r| {
        r.min_sample_rate().0 <= requested_rate
            && r.max_sample_rate().0 >= requested_rate
            && r.sample_format() == sample_fmt_to_cpal(requested_fmt)
    }) {
        return Ok((range.clone(), requested_rate, requested_fmt));
    }

    // 2. 尝试匹配采样率，接受任意格式
    if let Some(range) = ranges.iter().find(|r| {
        r.min_sample_rate().0 <= requested_rate && r.max_sample_rate().0 >= requested_rate
    }) {
        let used_fmt = cpal_to_sample_fmt(range.sample_format())?;
        return Ok((range.clone(), requested_rate, used_fmt));
    }

    // 3. 尝试匹配格式，接受任意采样率（取最接近的）
    if let Some(range) = ranges
        .iter()
        .find(|r| r.sample_format() == sample_fmt_to_cpal(requested_fmt))
    {
        let used_rate = find_nearest_sample_rate(
            range.min_sample_rate().0,
            range.max_sample_rate().0,
            requested_rate,
        );
        return Ok((range.clone(), used_rate, requested_fmt));
    }

    // 4. 最后一个手段：使用第一个配置，接受其任意格式和采样率
    let range = &ranges[0];
    let used_fmt = cpal_to_sample_fmt(range.sample_format())?;
    let used_rate = find_nearest_sample_rate(
        range.min_sample_rate().0,
        range.max_sample_rate().0,
        requested_rate,
    );
    Ok((range.clone(), used_rate, used_fmt))
}

/// 将我们的 SampleFmt 转换为 cpal 的 SampleFormat
fn sample_fmt_to_cpal(fmt: SampleFmt) -> cpal::SampleFormat {
    match fmt {
        SampleFmt::S16 => SampleFormat::I16,
        SampleFmt::S32 => SampleFormat::I32,
        SampleFmt::F32 => SampleFormat::F32,
    }
}

/// 将 cpal 的 SampleFormat 转换为我们 的 SampleFmt
fn cpal_to_sample_fmt(cpal_fmt: cpal::SampleFormat) -> Result<SampleFmt, String> {
    match cpal_fmt {
        SampleFormat::I16 => Ok(SampleFmt::S16),
        SampleFormat::I32 => Ok(SampleFmt::S32),
        SampleFormat::F32 => Ok(SampleFmt::F32),
        _ => Err(format!("不支持的采样格式: {:?}", cpal_fmt)),
    }
}

/// 找到最接近目标值的采样率
fn find_nearest_sample_rate(min: u32, max: u32, target: u32) -> u32 {
    if target <= min {
        return min;
    }
    if target >= max {
        return max;
    }
    // 常见采样率列表
    const COMMON_RATES: &[u32] = &[8000, 11025, 16000, 22050, 32000, 44100, 48000, 96000];

    // 找到最接近的常用采样率
    COMMON_RATES
        .iter()
        .copied()
        .filter(|&r| r >= min && r <= max)
        .min_by_key(|&r| r.abs_diff(target))
        .unwrap_or(target)
}
