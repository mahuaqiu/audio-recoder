mod capture;

use capture::{RecordConfig, SampleFmt, Source};
use std::sync::{mpsc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

// 记录文件路径
fn get_pid_file(output_path: &str) -> PathBuf {
    let stem = PathBuf::from(output_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{}.pid", stem))
}

fn get_stop_file(output_path: &str) -> PathBuf {
    let stem = PathBuf::from(output_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{}.stop", stem))
}

fn main() {
    ctrlc::set_handler(|| {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
    }).expect("设置 Ctrl+C 处理失败");

    let config = parse_args().unwrap_or_else(|e| {
        eprintln!("错误: {e}");
        eprintln!();
        print_usage();
        std::process::exit(1);
    });

    if let Err(e) = run(config) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run(config: RecordConfig) -> Result<(), String> {
    let source_name = match config.source {
        Source::Microphone => "麦克风",
        Source::Speaker => "扬声器",
    };
    eprintln!(
        "正在录制... (源: {}, 采样率: {}Hz, 格式: {}, 时长: {}s)",
        source_name,
        config.sample_rate,
        config.sample_fmt.as_str(),
        config.duration_secs,
    );

    // 创建 channel 用于音频数据传输
    let (tx, rx) = mpsc::channel();

    // 启动录制
    let stop_handle = match config.source {
        Source::Microphone => capture::record_microphone(&config, tx)?,
        Source::Speaker => capture::record_speaker(&config, tx)?,
    };

    // 检查扬声器是否初始化成功（麦克风失败在上面已经抛出错误了）
    if !stop_handle.is_recording() {
        return Err("音频录制初始化失败".to_string());
    }

    // 启动 WAV 写入线程
    let output_path = config.output_path.clone();
    let sample_rate = config.sample_rate;
    let sample_fmt = config.sample_fmt;
    let writer_thread = std::thread::spawn(move || {
        wav_writer_loop(rx, &output_path, sample_rate, sample_fmt)
    });

    // 前台模式：阻塞等待录制完成
    if config.foreground {
        let start = Instant::now();
        let duration = Duration::from_secs(config.duration_secs);

        while start.elapsed() < duration && !STOP_REQUESTED.load(Ordering::Relaxed) {
            let elapsed = start.elapsed().as_secs();
            eprint!("\r已录制: {}s / {}s", elapsed, config.duration_secs);
            std::thread::sleep(Duration::from_millis(500));
        }
        eprintln!();

        // 停止录制（drop stop_handle 会停止音频流）
        drop(stop_handle);

        // drop tx 已随 stop_handle drop 完成，channel 关闭后 writer 线程退出
        let _ = writer_thread.join();

        eprintln!("录制完成: {}", config.output_path);
    } else {
        // 后台模式：启动后立即返回
        
        // 创建 PID 文件和停止文件
        let pid_file = get_pid_file(&config.output_path);
        let stop_file = get_stop_file(&config.output_path);
        
        // 写入 PID
        let pid = std::process::id();
        fs::write(&pid_file, format!("{}", pid)).map_err(|e| format!("写入 PID 文件失败: {e}"))?;
        
        // 创建停止文件（存在表示需要继续录制）
        fs::write(&stop_file, "running").map_err(|e| format!("创建停止文件失败: {e}"))?;

        eprintln!("后台录制已启动，PID: {}", pid);
        eprintln!("输出文件: {}", config.output_path);
        eprintln!("PID 文件: {}", pid_file.display());
        eprintln!("停止文件: {} (删除此文件可停止录制)", stop_file.display());
        eprintln!();
        eprintln!("停止录制的方式:");
        eprintln!("  1. 删除停止文件: rm {}", stop_file.display());
        eprintln!("  2. 发送信号: kill -INT {}", pid);
        eprintln!("  3. 杀进程: kill {}", pid);

        // 后台模式：监控停止文件
        let stop_flag = std::sync::Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        // 启动监控线程
        std::thread::spawn(move || {
            // 等待 Ctrl+C 或停止文件被删除
            while !STOP_REQUESTED.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                
                // 检查停止文件是否还存在
                if !stop_file.exists() {
                    eprintln!("\n检测到停止文件已删除，正在停止...");
                    break;
                }
            }
            stop_flag_clone.store(true, Ordering::Relaxed);
        });

        // 等待 stop_flag 被设置
        while !stop_flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }

        // 停止录制
        drop(stop_handle);
        let _ = writer_thread.join();

        // 清理文件
        let _ = fs::remove_file(&pid_file);
        let _ = fs::remove_file(&stop_file);

        eprintln!("录制完成: {}", config.output_path);
    }

    Ok(())
}

/// WAV 写入循环
fn wav_writer_loop(
    rx: mpsc::Receiver<Vec<f64>>,
    output_path: &str,
    sample_rate: u32,
    sample_fmt: SampleFmt,
) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: sample_fmt.bits_per_sample(),
        sample_format: sample_fmt.to_hound_sample_format(),
    };

    let mut writer = hound::WavWriter::create(output_path, spec)
        .map_err(|e| format!("创建 WAV 文件失败: {e}"))?;

    while let Ok(samples) = rx.recv() {
        match sample_fmt {
            SampleFmt::S16 => {
                for s in &samples {
                    let val = (*s).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    writer.write_sample(val).map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
            SampleFmt::S32 => {
                for s in &samples {
                    let val = (*s).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                    writer.write_sample(val).map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
            SampleFmt::F32 => {
                for s in &samples {
                    writer.write_sample(*s as f32).map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
        }
    }

    writer.flush().map_err(|e| format!("flush WAV 文件失败: {e}"))?;
    Ok(())
}

fn print_usage() {
    eprintln!("用法: audio-recorder [选项]");
    eprintln!();
    eprintln!("选项:");
    eprintln!("  -s, --source <SOURCE>     音频源: microphone | speaker (默认: microphone)");
    eprintln!("  -r, --sample-rate <RATE>  采样率 (默认: 16000)");
    eprintln!("  -f, --sample-fmt <FMT>    采样格式: s16 | s32 | f32 (默认: s16)");
    eprintln!("  -d, --duration <SECS>     录制时长秒数 (默认: 120)");
    eprintln!("  -o, --output <PATH>       输出文件路径 (默认: recording.wav)");
    eprintln!("  -i, --device <INDEX>      输入设备索引 (默认: 系统默认设备)");
    eprintln!("  -b, --blocking            前台阻塞模式，等待录制完成 (默认: 后台运行)");
    eprintln!("  -h, --help                显示帮助信息");
    eprintln!();
    eprintln!("示例:");
    eprintln!("  audio-recorder                           # 后台录制麦克风");
    eprintln!("  audio-recorder -s speaker                # 录制扬声器");
    eprintln!("  audio-recorder -b                        # 前台阻塞模式");
    eprintln!("  audio-recorder -i 1 -o my.wav            # 指定设备1");
    eprintln!();
    eprintln!("停止后台录制:");
    eprintln!("  rm .recording.stop                       # 删除停止文件");
    eprintln!("  kill -INT <PID>                          # 发送中断信号");
}

fn parse_args() -> Result<RecordConfig, String> {
    use lexopt::prelude::*;

    let mut config = RecordConfig::default();
    let mut parser = lexopt::Parser::from_env();

    while let Some(arg) = parser.next().map_err(|e| format!("参数解析错误: {e}"))? {
        match arg {
            Short('s') | Long("source") => {
                let val: OsString = parser.value().map_err(|e| format!("--source 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.source = match val.as_str() {
                    "microphone" | "mic" => Source::Microphone,
                    "speaker" | "spk" => Source::Speaker,
                    _ => return Err(format!("未知音频源: {val}，支持: microphone, speaker")),
                };
            }
            Short('r') | Long("sample-rate") => {
                let val: OsString = parser.value().map_err(|e| format!("--sample-rate 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.sample_rate = val.parse().map_err(|_| format!("无效的采样率: {val}"))?;
            }
            Short('f') | Long("sample-fmt") => {
                let val: OsString = parser.value().map_err(|e| format!("--sample-fmt 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.sample_fmt = SampleFmt::from_str(&val)
                    .ok_or_else(|| format!("无效的采样格式: {val}，支持: s16, s32, f32"))?;
            }
            Short('d') | Long("duration") => {
                let val: OsString = parser.value().map_err(|e| format!("--duration 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.duration_secs = val.parse().map_err(|_| format!("无效的录制时长: {val}"))?;
            }
            Short('o') | Long("output") => {
                let val: OsString = parser.value().map_err(|e| format!("--output 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.output_path = val;
            }
            Short('i') | Long("device") => {
                let val: OsString = parser.value().map_err(|e| format!("--device 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.device_index = Some(val.parse().map_err(|_| format!("无效的设备索引: {val}"))?);
            }
            Short('b') | Long("blocking") => {
                config.foreground = true;
            }
            Short('h') | Long("help") => {
                print_usage();
                std::process::exit(0);
            }
            _ => return Err(format!("未知参数: {:?}", arg)),
        }
    }

    Ok(config)
}
