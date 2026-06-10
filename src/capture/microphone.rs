use crate::capture::{RecordConfig, SampleFmt};
use std::sync::mpsc;

/// 录制停止句柄，drop 时停止录制
pub struct StopHandle {
    _stream: cpal::Stream,
}

/// 使用 cpal 录制麦克风音频
/// 采样数据通过 tx 发送，返回停止句柄
pub fn record_microphone(
    config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("未找到麦克风设备")?;

    eprintln!("麦克风设备: {}", device.name().unwrap_or_default());

    let supported_config = device
        .supported_input_configs()
        .map_err(|e| format!("查询设备配置失败: {e}"))?
        .find(|c| {
            c.min_sample_rate().0 <= config.sample_rate
                && c.max_sample_rate().0 >= config.sample_rate
                && sample_fmt_matches(c.sample_format(), config.sample_fmt)
        })
        .ok_or(format!(
            "设备不支持 采样率={} 格式={}",
            config.sample_rate,
            config.sample_fmt.as_str()
        ))?;

    // 使用最接近请求配置的流配置
    let stream_config = supported_config
        .with_sample_rate(cpal::SampleRate(config.sample_rate))
        .config();

    let err_fn = |err: cpal::StreamError| {
        eprintln!("音频流错误: {err}");
    };

    // 根据采样格式构建对应的流
    let stream = match config.sample_fmt {
        SampleFmt::F32 => device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<f64> = data.iter().map(|&s| s as f64).collect();
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
                    let samples: Vec<f64> = data.iter().map(|&s| s as f64).collect();
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
                    let samples: Vec<f64> = data.iter().map(|&s| s as f64).collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("创建音频流失败: {e}"))?,
    };

    stream.play().map_err(|e| format!("启动音频流失败: {e}"))?;

    Ok(StopHandle { _stream: stream })
}

fn sample_fmt_matches(cpal_fmt: cpal::SampleFormat, expected: SampleFmt) -> bool {
    matches!(
        (cpal_fmt, expected),
        (cpal::SampleFormat::I16, SampleFmt::S16)
            | (cpal::SampleFormat::I32, SampleFmt::S32)
            | (cpal::SampleFormat::F32, SampleFmt::F32)
    )
}
