//! macOS 扬声器录制 - 使用 ScreenCaptureKit 捕获系统音频
//! 需要 macOS 13.0+ 和屏幕录制权限

use crate::capture::{RecordConfig, StopHandle};
use screencapturekit::prelude::*;
use std::sync::mpsc;
use std::sync::{Arc, AtomicBool};
use std::thread;
use std::time::Duration;

/// macOS 系统音频录制（使用 ScreenCaptureKit）
/// 注意：macOS 13.0+ 需要屏幕录制权限
pub fn record_speaker(
    config: &RecordConfig,
    tx: mpsc::Sender<Vec<f64>>,
) -> Result<StopHandle, String> {
    // 创建停止标志
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 克隆 tx 和 stop_flag 用于线程中
    let tx_clone = tx.clone();
    let stop_flag_clone = stop_flag.clone();

    // 创建 tokio 运行时
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建 tokio 运行时失败: {e}"))?;

    // 在新线程中运行录制（ScreenCaptureKit 是异步 API）
    thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            // 获取可共享的内容（需要权限）
            let content = rt.block_on(async {
                SCShareableContent::get()
            }).map_err(|e| format!("获取屏幕内容失败，请确认已授予屏幕录制权限: {e}"))?;

            // 获取第一个显示器
            let display = content.displays()
                .into_iter()
                .next()
                .ok_or("未找到可用的显示器")?;

            // 创建内容过滤器 - 只需要系统音频，不需要视频
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();

            // 配置流 - 只启用音频捕获
            let mut stream_config = SCStreamConfiguration::new();
            stream_config.set_captures_audio(true);
            
            // 设置采样率
            let sample_rate = AudioSampleRate::from_hz(config.sample_rate as i32)
                .unwrap_or(AudioSampleRate::Rate48000);
            stream_config.set_sample_rate(sample_rate);
            
            // 设置单声道
            stream_config.set_audio_channel_count(AudioChannelCount::Mono);

            // 创建流
            let mut sc_stream = SCStream::new(&filter, &stream_config)
                .map_err(|e| format!("创建 SCStream 失败: {e}"))?;

            // 添加音频输出 handler
            let tx_inner = tx_clone.clone();
            sc_stream.add_output_handler(
                move |sample: CMSampleBuffer, of_type: SCStreamOutputType| {
                    if of_type != SCStreamOutputType::Audio {
                        return;
                    }

                    // 从 sample buffer 获取音频数据
                    if let Some(audio_buffer_list) = sample.audio_buffer_list() {
                        for buffer in audio_buffer_list.iter() {
                            let data = buffer.data();
                            let bytes_per_sample = 4; // float32
                            
                            // 将 bytes 转换为 f64 样本
                            let samples: Vec<f64> = data
                                .chunks_exact(bytes_per_sample)
                                .map(|chunk| {
                                    let val = f32::from_le_bytes(chunk.try_into().unwrap());
                                    val as f64
                                })
                                .collect();

                            if !samples.is_empty() {
                                let _ = tx_inner.send(samples);
                            }
                        }
                    }
                },
                SCStreamOutputType::Audio,
            );

            // 启动流
            rt.block_on(async {
                sc_stream.start()
            }).map_err(|e| format!("启动流失败: {e}"))?;

            // 保持运行直到收到停止信号
            while !stop_flag_clone.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }

            // 停止流
            rt.block_on(async {
                sc_stream.stop()
            }).map_err(|e| format!("停止流失败: {e}"))?;

            Ok(())
        })();

        if let Err(e) = result {
            eprintln!("扬声器录制错误: {e}");
        }
    });

    eprintln!("正在录制系统音频... (需要 macOS 13.0+ 和屏幕录制权限)");

    // 返回一个带有停止标志的 StopHandle
    Ok(StopHandle::new_speaker(stop_flag))
}
