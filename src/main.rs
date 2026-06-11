mod capture;

use capture::{RecordConfig, SampleFmt, Source};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

// 记录文件路径
fn get_pid_file(output_path: &str) -> PathBuf {
    let path = PathBuf::from(output_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{}.pid", stem))
}

fn get_stop_file(output_path: &str) -> PathBuf {
    let path = PathBuf::from(output_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{}.stop", stem))
}

fn main() {
    ctrlc::set_handler(|| {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
    })
    .expect("设置 Ctrl+C 处理失败");

    match parse_args() {
        Ok(Action::ListDevices) => {
            list_devices();
        }
        Ok(Action::Record(config)) => {
            if let Err(e) = run(config) {
                eprintln!("错误: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("错误: {e}");
            eprintln!();
            print_usage();
            std::process::exit(1);
        }
    }
}

/// 列出所有可用音频设备（输入+输出）
fn list_devices() {
    // 列出麦克风设备
    match capture::list_input_devices() {
        Ok(devices) => {
            if devices.is_empty() {
                eprintln!("没有可用的麦克风设备");
            } else {
                eprintln!("麦克风设备 (输入):");
                for (i, name) in devices.iter().enumerate() {
                    eprintln!("  [{}] {}", i, name);
                }
            }
        }
        Err(e) => {
            eprintln!("枚举麦克风设备失败: {e}");
        }
    }
    
    eprintln!();
    
    // 列出扬声器设备
    match capture::list_output_devices() {
        Ok(devices) => {
            if devices.is_empty() {
                eprintln!("没有可用的扬声器设备");
            } else {
                eprintln!("扬声器设备 (输出):");
                for (i, name) in devices.iter().enumerate() {
                    eprintln!("  [{}] {}", i, name);
                }
            }
        }
        Err(e) => {
            eprintln!("枚举扬声器设备失败: {e}");
        }
    }
}

/// 命令行动作
enum Action {
    /// 列出设备
    ListDevices,
    /// 录制
    Record(RecordConfig),
}

fn run(config: RecordConfig) -> Result<(), String> {
    // 后台模式：直接 spawn 子进程以前台模式运行，父进程不初始化音频
    if !config.foreground {
        return run_background(&config);
    }

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

    // 获取实际使用的采样率和格式（设备可能自动适配了）
    let actual_sample_rate = stop_handle.actual_sample_rate;
    let actual_sample_fmt = stop_handle.actual_sample_fmt;
    let target_sample_rate = config.sample_rate;
    let target_sample_fmt = config.sample_fmt;

    // 如果实际参数与请求参数不同，提示用户
    if actual_sample_rate != config.sample_rate || actual_sample_fmt != config.sample_fmt {
        eprintln!(
            "提示: 自动适配后，设备实际使用 {}Hz {}，\n      将重采样转换为目标 {}Hz {}",
            actual_sample_rate,
            actual_sample_fmt.as_str(),
            target_sample_rate,
            target_sample_fmt.as_str()
        );
    }

    // 启动 WAV 写入线程
    let output_path = config.output_path.clone();
    let writer_thread = std::thread::spawn(move || {
        wav_writer_loop(
            rx,
            &output_path,
            actual_sample_rate,
            target_sample_rate,
            target_sample_fmt,
        )
    });

    // 前台模式：阻塞等待录制完成
    let start = Instant::now();
    let duration = Duration::from_secs(config.duration_secs);
    let stop_file = get_stop_file(&config.output_path);
    let pid_file = get_pid_file(&config.output_path);

    // 如果停止文件存在，说明是后台子进程，支持通过删除停止文件来停止
    let has_stop_file = stop_file.exists();

    while start.elapsed() < duration && !STOP_REQUESTED.load(Ordering::Relaxed) {
        let elapsed = start.elapsed().as_secs();
        eprint!("\r已录制: {}s / {}s", elapsed, config.duration_secs);

        // 检查停止文件是否被删除
        if has_stop_file && !stop_file.exists() {
            eprintln!("\n检测到停止文件已删除，正在停止...");
            break;
        }

        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!();

    // 停止录制（drop stop_handle 会停止音频流）
    drop(stop_handle);

    // drop tx 已随 stop_handle drop 完成，channel 关闭后 writer 线程退出
    let _ = writer_thread.join();

    // 清理 PID 和停止文件
    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_file(&stop_file);

    eprintln!("录制完成: {}", config.output_path);

    Ok(())
}

/// 后台模式：spawn 子进程以前台模式运行，父进程立即退出
fn run_background(config: &RecordConfig) -> Result<(), String> {
    let pid_file = get_pid_file(&config.output_path);
    let stop_file = get_stop_file(&config.output_path);

    // 构建子进程命令：加上 -b 参数，其余参数保持不变
    let current_exe =
        std::env::current_exe().map_err(|e| format!("获取当前可执行文件路径失败: {e}"))?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("-b"); // 子进程用前台模式
    if config.source == Source::Speaker {
        cmd.arg("-s").arg("speaker");
    }
    cmd.arg("-r").arg(config.sample_rate.to_string());
    cmd.arg("-f").arg(config.sample_fmt.as_str());
    cmd.arg("-d").arg(config.duration_secs.to_string());
    cmd.arg("-o").arg(&config.output_path);
    if let Some(ref name) = config.device_name {
        cmd.arg("-i").arg(name);
    }

    // 子进程的标准输入/输出/错误分离，不阻塞父进程
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // 创建停止文件（在子进程启动前创建，确保子进程能检测到）
    fs::write(&stop_file, "running").map_err(|e| format!("创建停止文件失败: {e}"))?;

    let child = cmd.spawn().map_err(|e| {
        // 启动失败，清理停止文件
        let _ = fs::remove_file(&stop_file);
        format!("启动后台子进程失败: {e}")
    })?;
    let child_pid = child.id();

    // 写入 PID 文件
    fs::write(&pid_file, format!("{}", child_pid))
        .map_err(|e| format!("写入 PID 文件失败: {e}"))?;

    let source_name = match config.source {
        Source::Microphone => "麦克风",
        Source::Speaker => "扬声器",
    };
    eprintln!("后台录制已启动，PID: {}", child_pid);
    eprintln!(
        "录制参数: 源={}, 采样率={}Hz, 格式={}, 时长={}s",
        source_name,
        config.sample_rate,
        config.sample_fmt.as_str(),
        config.duration_secs
    );
    eprintln!("输出文件: {}", config.output_path);
    eprintln!("PID 文件: {}", pid_file.display());
    eprintln!("停止文件: {} (删除此文件可停止录制)", stop_file.display());
    eprintln!();
    eprintln!("停止录制的方式:");
    eprintln!("  1. 删除停止文件: rm {}", stop_file.display());
    eprintln!("  2. 发送信号: kill -INT {}", child_pid);
    eprintln!("  3. 杀进程: kill {}", child_pid);

    // 父进程立即退出，子进程在后台运行
    // 不 wait 子进程，让它独立运行
    std::mem::forget(child); // 防止 Drop 时 wait

    Ok(())
}

/// WAV 写入循环
fn wav_writer_loop(
    rx: mpsc::Receiver<Vec<f64>>,
    output_path: &str,
    actual_rate: u32,
    target_rate: u32,
    target_fmt: SampleFmt,
) -> Result<(), String> {
    // 使用实际采样率写 WAV header，但用目标格式写数据
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: target_rate,
        bits_per_sample: target_fmt.bits_per_sample(),
        sample_format: target_fmt.to_hound_sample_format(),
    };

    let mut writer = hound::WavWriter::create(output_path, spec)
        .map_err(|e| format!("创建 WAV 文件失败: {e}"))?;

    while let Ok(samples) = rx.recv() {
        // 重采样到目标采样率
        let resampled = if actual_rate != target_rate {
            resample(&samples, actual_rate, target_rate)
        } else {
            samples
        };

        // 转换为目标格式并写入
        match target_fmt {
            SampleFmt::S16 => {
                for s in &resampled {
                    let val = (*s * 32767.0).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    writer
                        .write_sample(val)
                        .map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
            SampleFmt::S32 => {
                for s in &resampled {
                    let val = (*s * 2147483647.0).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                    writer
                        .write_sample(val)
                        .map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
            SampleFmt::F32 => {
                for s in &resampled {
                    writer
                        .write_sample(*s as f32)
                        .map_err(|e| format!("写入采样失败: {e}"))?;
                }
            }
        }
    }

    writer
        .flush()
        .map_err(|e| format!("flush WAV 文件失败: {e}"))?;
    Ok(())
}

/// 线性插值重采样
fn resample(samples: &[f64], from_rate: u32, to_rate: u32) -> Vec<f64> {
    if samples.is_empty() || from_rate == to_rate {
        return samples.to_vec();
    }

    let ratio = to_rate as f64 / from_rate as f64;
    let new_len = ((samples.len() as f64) * ratio).ceil() as usize;
    let mut result = Vec::with_capacity(new_len);

    for i in 0..new_len {
        let src_idx = i as f64 / ratio;
        let idx0 = src_idx.floor() as usize;
        let idx1 = (idx0 + 1).min(samples.len() - 1);
        let frac = src_idx.fract();

        let val = samples[idx0] * (1.0 - frac) + samples[idx1] * frac;
        result.push(val);
    }

    result
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
    eprintln!("  -i, --device <NAME>       输入设备名称 (模糊匹配, 默认: 系统默认设备)");
eprintln!("  -l, --list-devices        列出可用输入设备");
    eprintln!("  -b, --blocking            前台阻塞模式，等待录制完成 (默认: 后台运行)");
    eprintln!("  -h, --help                显示帮助信息");
    eprintln!();
    eprintln!("示例:");
    eprintln!("  audio-recorder                           # 后台录制麦克风");
    eprintln!("  audio-recorder -s speaker                # 录制扬声器");
    eprintln!("  audio-recorder -b                        # 前台阻塞模式");
    eprintln!("  audio-recorder -i MacBook -o my.wav         # 模糊匹配设备名");
    eprintln!();
    eprintln!("停止后台录制:");
    eprintln!("  rm .recording.stop                       # 删除停止文件");
    eprintln!("  kill -INT <PID>                          # 发送中断信号");
}

fn parse_args() -> Result<Action, String> {
    use lexopt::prelude::*;

    let mut config = RecordConfig::default();
    let mut parser = lexopt::Parser::from_env();

    while let Some(arg) = parser.next().map_err(|e| format!("参数解析错误: {e}"))? {
        match arg {
            Short('s') | Long("source") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--source 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.source = match val.as_str() {
                    "microphone" | "mic" => Source::Microphone,
                    "speaker" | "spk" => Source::Speaker,
                    _ => return Err(format!("未知音频源: {val}，支持: microphone, speaker")),
                };
            }
            Short('r') | Long("sample-rate") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--sample-rate 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.sample_rate = val.parse().map_err(|_| format!("无效的采样率: {val}"))?;
            }
            Short('f') | Long("sample-fmt") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--sample-fmt 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.sample_fmt = SampleFmt::from_str(&val)
                    .ok_or_else(|| format!("无效的采样格式: {val}，支持: s16, s32, f32"))?;
            }
            Short('d') | Long("duration") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--duration 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.duration_secs = val.parse().map_err(|_| format!("无效的录制时长: {val}"))?;
            }
            Short('o') | Long("output") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--output 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.output_path = val;
            }
            Short('i') | Long("device") => {
                let val: OsString = parser
                    .value()
                    .map_err(|e| format!("--device 需要参数: {e}"))?;
                let val = val.to_string_lossy().into_owned();
                config.device_name = Some(val);
            }
            Short('l') | Long("list-devices") => {
                return Ok(Action::ListDevices);
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

    Ok(Action::Record(config))
}
