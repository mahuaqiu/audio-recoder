mod capture;
pub mod fsk_marker;
mod timing;

use capture::{CapturedPacket, RecordConfig, SampleFmt, Source};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use timing::{
    write_wav_and_sidecar_atomic, Discontinuity, QpcUtcMapper, TimeSyncSidecarV2, TimingAnchor,
    TimingSidecarV2, ValidatedTimeSyncReport,
};

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

enum Action {
    ListDevices,
    Record(RecordConfig),
}

fn now_unix_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn pid_file(output: &str) -> PathBuf {
    let stem = Path::new(output)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{stem}.pid"))
}

fn stop_file(output: &str) -> PathBuf {
    let stem = Path::new(output)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("recording");
    PathBuf::from(format!(".{stem}.stop"))
}

fn main() {
    ctrlc::set_handler(|| {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
    })
    .expect("设置 Ctrl+C 处理失败");
    match parse_args() {
        Ok(Action::ListDevices) => list_devices(),
        Ok(Action::Record(config)) => {
            if let Err(error) = run(config) {
                eprintln!("错误: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("错误: {error}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn list_devices() {
    match capture::list_input_devices() {
        Ok(devices) => {
            eprintln!("麦克风设备:");
            for (index, device) in devices.iter().enumerate() {
                eprintln!("  [{index}] {device}");
            }
        }
        Err(error) => eprintln!("枚举麦克风失败: {error}"),
    }
    eprintln!();
    match capture::list_output_devices() {
        Ok(devices) => {
            eprintln!("扬声器设备:");
            for (index, device) in devices.iter().enumerate() {
                eprintln!("  [{index}] {device}");
            }
        }
        Err(error) => eprintln!("枚举扬声器失败: {error}"),
    }
}

fn run(config: RecordConfig) -> Result<(), String> {
    if !config.foreground {
        return run_background(&config);
    }
    let validated_sync = validate_time_sync(&config)?;
    if config.timestamp_mark && config.source != Source::Speaker {
        return Err(
            "新 timing 协议只支持 Windows WASAPI speaker loopback，请使用 --source speaker".into(),
        );
    }
    if config.timestamp_mark && config.sample_rate < 16_000 {
        return Err("时间标记要求目标采样率不低于 16000Hz".into());
    }

    let source_name = match config.source {
        Source::Microphone => "麦克风",
        Source::Speaker => "扬声器",
    };
    eprintln!(
        "正在录制... (源: {source_name}, 目标采样率: {}Hz, 格式: {}, 时长: {}s)",
        config.sample_rate,
        config.sample_fmt.as_str(),
        config.duration_secs
    );
    let (tx, rx) = mpsc::channel::<CapturedPacket>();
    let stop_handle = match config.source {
        Source::Microphone => capture::record_microphone(&config, tx)?,
        Source::Speaker => capture::record_speaker(&config, tx)?,
    };
    if !stop_handle.is_recording() {
        return Err("音频录制初始化失败".into());
    }

    let stop_marker = stop_file(&config.output_path);
    let pid_marker = pid_file(&config.output_path);
    if stop_marker.exists() {
        fs::write(&pid_marker, std::process::id().to_string())
            .map_err(|e| format!("创建 PID 文件失败: {e}"))?;
    }
    if stop_handle.actual_sample_rate != config.sample_rate
        || stop_handle.actual_sample_fmt != config.sample_fmt
    {
        eprintln!(
            "提示: 设备实际参数为 {}Hz {}，将全局重采样为 {}Hz {}",
            stop_handle.actual_sample_rate,
            stop_handle.actual_sample_fmt.as_str(),
            config.sample_rate,
            config.sample_fmt.as_str()
        );
    }

    let mut mapper = QpcUtcMapper::new();
    let recording_started_unix_ns = now_unix_ns();
    if config.timestamp_mark {
        mapper.capture("start")?;
    }
    let started = Instant::now();
    let source_rate = stop_handle.actual_sample_rate;
    let has_stop_marker = stop_marker.exists();
    let mut last_cal = Instant::now();
    while started.elapsed() < Duration::from_secs(config.duration_secs)
        && !STOP_REQUESTED.load(Ordering::Relaxed)
    {
        eprint!(
            "\r已录制 {}s / {}s",
            started.elapsed().as_secs(),
            config.duration_secs
        );
        if has_stop_marker && !stop_marker.exists() {
            eprintln!("\n检测到停止文件已删除，正在停止...");
            break;
        }
        if config.timestamp_mark && last_cal.elapsed() >= Duration::from_secs(1) {
            let _ = mapper.capture("periodic");
            last_cal = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!();
    drop(stop_handle);
    let _ = fs::remove_file(&pid_marker);
    let _ = fs::remove_file(&stop_marker);
    if config.timestamp_mark {
        mapper.capture("end")?;
    }
    let recording_ended_unix_ns = now_unix_ns();
    let packets: Vec<CapturedPacket> = rx.into_iter().collect();
    write_recording(
        &config,
        packets,
        source_rate,
        mapper,
        validated_sync,
        recording_started_unix_ns,
        recording_ended_unix_ns,
    )
}

fn run_background(config: &RecordConfig) -> Result<(), String> {
    let current_exe = std::env::current_exe().map_err(|e| format!("获取可执行文件失败: {e}"))?;
    let mut command = std::process::Command::new(current_exe);
    command.arg("-b");
    if config.source == Source::Speaker {
        command.args(["-s", "speaker"]);
    }
    command.args(["-r", &config.sample_rate.to_string()]);
    command.args(["-f", config.sample_fmt.as_str()]);
    command.args(["-d", &config.duration_secs.to_string()]);
    command.args(["-o", &config.output_path]);
    if let Some(device) = &config.device_name {
        command.args(["-i", device]);
    }
    if config.timestamp_mark {
        command.arg("-t");
    }
    if let Some(report) = &config.time_sync_report {
        command.args(["--time-sync-report", report]);
    }
    if config.require_time_sync {
        command.arg("--require-time-sync");
    }
    command.args([
        "--max-clock-offset",
        &config.max_clock_offset_ms.to_string(),
    ]);
    command.args([
        "--max-sync-report-age",
        &config.max_sync_report_age_secs.to_string(),
    ]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let marker = stop_file(&config.output_path);
    fs::write(&marker, "running").map_err(|e| format!("创建停止文件失败: {e}"))?;
    let child = command.spawn().map_err(|e| {
        let _ = fs::remove_file(&marker);
        format!("启动后台录音失败: {e}")
    })?;
    let child_pid = child.id();
    let pid = pid_file(&config.output_path);
    let ready = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        fs::read_to_string(&pid)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            == Some(child_pid)
    });
    if !ready {
        let _ = fs::remove_file(&marker);
        let _ = fs::remove_file(&pid);
        return Err("后台录音初始化失败，请检查设备和时间同步报告".into());
    }
    eprintln!("后台录音已启动，PID: {child_pid}");
    eprintln!("输出文件: {}", config.output_path);
    eprintln!("停止文件: {}", marker.display());
    std::mem::forget(child);
    Ok(())
}

fn validate_time_sync(config: &RecordConfig) -> Result<Option<ValidatedTimeSyncReport>, String> {
    let Some(path) = &config.time_sync_report else {
        if config.timestamp_mark || config.require_time_sync {
            return Err("启用新 timing 协议时必须提供 --time-sync-report".into());
        }
        return Ok(None);
    };
    if !(config.timestamp_mark || config.require_time_sync) {
        return Ok(None);
    }
    let report = timing::load_and_validate_pre_sync(
        Path::new(path),
        config.max_clock_offset_ms,
        config.max_sync_report_age_secs,
        now_unix_ns(),
    )?;
    Ok(Some(report))
}

fn write_recording(
    config: &RecordConfig,
    packets: Vec<CapturedPacket>,
    source_rate: u32,
    mapper: QpcUtcMapper,
    validated_sync: Option<ValidatedTimeSyncReport>,
    recording_started_unix_ns: i64,
    recording_ended_unix_ns: i64,
) -> Result<(), String> {
    if packets.is_empty() {
        return Err("没有收到任何音频 packet".into());
    }
    if config.timestamp_mark && !cfg!(target_os = "windows") {
        return Err("新 timing 协议当前只支持 Windows".into());
    }
    let actual_rate = config.sample_rate;
    let (pcm, packet_offsets, first_qpc, _first_device, discontinuities) =
        collect_pcm(&packets, source_rate);
    let output_pcm = resample(&pcm, source_rate, actual_rate);

    let (first_utc_ns, first_millis) = if config.timestamp_mark {
        let qpc = first_qpc.ok_or("WASAPI 没有返回首个 PCM packet 的 QPC 时间")?;
        let utc_ns = mapper.map_qpc_to_utc_ns(qpc)?;
        (utc_ns, millis_of_day(utc_ns))
    } else {
        (0, 0)
    };
    let marker = if config.timestamp_mark {
        fsk_marker::encode_timestamp(first_millis, actual_rate)
    } else {
        Vec::new()
    };

    let mut anchors = Vec::new();
    if config.timestamp_mark {
        for (packet, offset) in packets.iter().zip(packet_offsets.iter().copied()) {
            if let (Some(device_position), Some(qpc)) = (packet.device_position, packet.qpc_100ns) {
                let wav_index =
                    (offset as f64 * actual_rate as f64 / source_rate as f64).round() as u64;
                let utc_ns = mapper.map_qpc_to_utc_ns(qpc)?;
                anchors.push(TimingAnchor {
                    wav_sample_index: wav_index,
                    device_position,
                    qpc_100ns: qpc,
                    utc_unix_ns: utc_ns,
                });
            }
        }
        if anchors.is_empty() {
            return Err("没有可用的 WASAPI timing anchor".into());
        }
        // 确保首 anchor 为真实 PCM 起点
        if let Some(first) = anchors.first_mut() {
            first.wav_sample_index = 0;
        }
        if mapper.clock_jump_detected() {
            return Err("检测到墙钟跳变，不产出正式录音产物".into());
        }
        if mapper.calibrations().len() < 2 {
            return Err("QPC/UTC 校准点不足（至少需要 start 与 end）".into());
        }
    }

    let parent = Path::new(&config.output_path)
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
    }

    let wav_path = PathBuf::from(&config.output_path);
    let wav_partial = PathBuf::from(format!("{}.partial", config.output_path));
    let _ = fs::remove_file(&wav_partial);
    let _ = fs::remove_file(format!("{}.timing.json", config.output_path));

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: actual_rate,
        bits_per_sample: config.sample_fmt.bits_per_sample(),
        sample_format: config.sample_fmt.to_hound_sample_format(),
    };
    {
        let mut writer = hound::WavWriter::create(&wav_partial, spec)
            .map_err(|e| format!("创建 WAV partial 失败: {e}"))?;
        for sample in marker.iter().chain(output_pcm.iter()) {
            write_sample(&mut writer, *sample, config.sample_fmt)?;
        }
        writer
            .finalize()
            .map_err(|e| format!("完成 WAV 写入失败: {e}"))?;
    }

    if config.timestamp_mark {
        let sync = validated_sync.ok_or("缺少已校验的时间同步报告")?;
        let wav_file = wav_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("recording.wav")
            .to_string();
        let sidecar = TimingSidecarV2 {
            schema_version: 2,
            clock_domain: "windows-utc-synchronized-by-ntp",
            source: "wasapi-loopback",
            wav_file,
            wav_sha256: String::new(),
            sample_rate: actual_rate,
            actual_device_sample_rate: source_rate,
            device_id: None,
            device_name: config.device_name.clone(),
            first_pcm_utc_unix_ns: first_utc_ns,
            first_pcm_millis_of_day: first_millis,
            fsk_semantics: "first_pcm_sample",
            fsk_prefix_samples: marker.len(),
            recording_started_unix_ns,
            recording_ended_unix_ns,
            qpc_utc_calibrations: mapper.calibrations().to_vec(),
            clock_jump_detected: mapper.clock_jump_detected(),
            anchors,
            discontinuities,
            time_sync: TimeSyncSidecarV2 {
                schema_version: sync.schema_version,
                report_kind: sync.report_kind,
                server: sync.ntp_server,
                checked_at_unix_ns: sync.checked_at_unix_ns,
                status: sync.status,
                max_abs_offset_ms: sync.max_abs_offset_ms,
                median_offset_ms: sync.median_offset_ms,
                rtt_p50_ms: sync.rtt_p50_ms,
            },
        };
        write_wav_and_sidecar_atomic(&wav_path, &wav_partial, sidecar)?;
    } else {
        fs::rename(&wav_partial, &wav_path).map_err(|e| format!("替换 WAV 失败: {e}"))?;
    }
    eprintln!("录制完成: {}", config.output_path);
    Ok(())
}

fn collect_pcm(
    packets: &[CapturedPacket],
    rate: u32,
) -> (
    Vec<f64>,
    Vec<usize>,
    Option<u64>,
    Option<u64>,
    Vec<Discontinuity>,
) {
    let first_device = packets.iter().find_map(|packet| packet.device_position);
    let first_qpc = packets.iter().find_map(|packet| packet.qpc_100ns);
    let mut pcm = Vec::new();
    let mut offsets = Vec::with_capacity(packets.len());
    let mut discontinuities = Vec::new();
    let mut cursor = 0usize;
    for packet in packets {
        let offset = match (first_device, packet.device_position) {
            (Some(first), Some(position)) if position >= first => (position - first) as usize,
            _ => cursor,
        };
        if offset > pcm.len() {
            discontinuities.push(Discontinuity {
                wav_sample_index: pcm.len() as u64,
                device_position: packet.device_position,
                flags: packet.flags,
                reason: "device-position-gap-filled-with-silence",
            });
            pcm.resize(offset, 0.0);
        }
        if offset < cursor && packet.flags != 0 {
            discontinuities.push(Discontinuity {
                wav_sample_index: (cursor as f64 * rate as f64 / rate as f64) as u64,
                device_position: packet.device_position,
                flags: packet.flags,
                reason: "device-position-overlap-or-discontinuity",
            });
        }
        offsets.push(offset);
        if offset == pcm.len() {
            pcm.extend_from_slice(&packet.samples);
        } else {
            let end = offset.saturating_add(packet.samples.len());
            if end > pcm.len() {
                pcm.resize(end, 0.0);
            }
            pcm[offset..end].copy_from_slice(&packet.samples);
        }
        cursor = cursor.max(offset.saturating_add(packet.samples.len()));
        if packet.flags & 0x1 != 0 || packet.flags & 0x4 != 0 {
            discontinuities.push(Discontinuity {
                wav_sample_index: offset as u64,
                device_position: packet.device_position,
                flags: packet.flags,
                reason: "wasapi-buffer-flag",
            });
        }
    }
    (pcm, offsets, first_qpc, first_device, discontinuities)
}

fn write_sample(
    writer: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    sample: f64,
    format: SampleFmt,
) -> Result<(), String> {
    match format {
        SampleFmt::S16 => {
            writer.write_sample((sample * 32767.0).clamp(i16::MIN as f64, i16::MAX as f64) as i16)
        }
        SampleFmt::S32 => writer.write_sample(
            (sample * 2_147_483_647.0).clamp(i32::MIN as f64, i32::MAX as f64) as i32,
        ),
        SampleFmt::F32 => writer.write_sample(sample as f32),
    }
    .map_err(|e| format!("写入 WAV 样本失败: {e}"))
}

fn resample(samples: &[f64], from_rate: u32, to_rate: u32) -> Vec<f64> {
    if samples.is_empty() || from_rate == to_rate {
        return samples.to_vec();
    }
    let output_len =
        ((samples.len() as f64 * to_rate as f64 / from_rate as f64).round() as usize).max(1);
    let ratio = to_rate as f64 / from_rate as f64;
    (0..output_len)
        .map(|index| {
            let source = index as f64 / ratio;
            let left = source.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            samples[left] * (1.0 - source.fract()) + samples[right] * source.fract()
        })
        .collect()
}

fn millis_of_day(unix_ns: i64) -> u32 {
    let seconds = unix_ns.div_euclid(1_000_000_000).rem_euclid(86_400);
    (seconds * 1000 + unix_ns.rem_euclid(1_000_000_000) / 1_000_000) as u32
}

fn print_usage() {
    eprintln!("用法: audio-recorder [选项]");
    eprintln!("  -s, --source <SOURCE>     microphone | speaker");
    eprintln!("  -r, --sample-rate <RATE>  目标采样率 (默认 16000)");
    eprintln!("  -f, --sample-fmt <FMT>    s16 | s32 | f32");
    eprintln!("  -d, --duration <SECS>     录制时长");
    eprintln!("  -o, --output <PATH>       WAV 输出路径");
    eprintln!("  -i, --device <NAME>       设备名模糊匹配");
    eprintln!("  -l, --list-devices        列出设备");
    eprintln!("  -b, --blocking            前台阻塞模式");
    eprintln!("  -t, --timestamp-mark      启用新 FSK 和 timing sidecar");
    eprintln!("      --time-sync-report <PATH>  时间同步报告");
    eprintln!("      --require-time-sync       要求同步报告通过");
    eprintln!("      --max-clock-offset <MS>   最大允许偏差，默认 5");
    eprintln!("      --max-sync-report-age <S> 同步报告有效期秒，默认 600");
}

fn parse_args() -> Result<Action, String> {
    use lexopt::prelude::*;
    let mut config = RecordConfig::default();
    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next().map_err(|e| format!("参数解析失败: {e}"))? {
        match arg {
            Short('s') | Long("source") => {
                let value = value_of(&mut parser, "--source")?;
                config.source = match value.as_str() {
                    "microphone" | "mic" => Source::Microphone,
                    "speaker" | "spk" => Source::Speaker,
                    _ => return Err(format!("未知音频源: {value}")),
                };
            }
            Short('r') | Long("sample-rate") => {
                config.sample_rate = value_of(&mut parser, "--sample-rate")?
                    .parse()
                    .map_err(|_| "无效采样率".to_string())?;
            }
            Short('f') | Long("sample-fmt") => {
                let value = value_of(&mut parser, "--sample-fmt")?;
                config.sample_fmt =
                    SampleFmt::from_str(&value).ok_or_else(|| format!("无效采样格式: {value}"))?;
            }
            Short('d') | Long("duration") => {
                config.duration_secs = value_of(&mut parser, "--duration")?
                    .parse()
                    .map_err(|_| "无效录制时长".to_string())?;
            }
            Short('o') | Long("output") => config.output_path = value_of(&mut parser, "--output")?,
            Short('i') | Long("device") => {
                config.device_name = Some(value_of(&mut parser, "--device")?)
            }
            Short('l') | Long("list-devices") => return Ok(Action::ListDevices),
            Short('b') | Long("blocking") => config.foreground = true,
            Short('t') | Long("timestamp-mark") => config.timestamp_mark = true,
            Long("time-sync-report") => {
                config.time_sync_report = Some(value_of(&mut parser, "--time-sync-report")?)
            }
            Long("require-time-sync") => config.require_time_sync = true,
            Long("max-clock-offset") => {
                config.max_clock_offset_ms = value_of(&mut parser, "--max-clock-offset")?
                    .parse()
                    .map_err(|_| "无效时钟偏差阈值".to_string())?
            }
            Long("max-sync-report-age") => {
                config.max_sync_report_age_secs = value_of(&mut parser, "--max-sync-report-age")?
                    .parse()
                    .map_err(|_| "无效同步报告有效期".to_string())?
            }
            Short('h') | Long("help") => {
                print_usage();
                std::process::exit(0);
            }
            _ => return Err(format!("未知参数: {arg:?}")),
        }
    }
    Ok(Action::Record(config))
}

fn value_of(parser: &mut lexopt::Parser, name: &str) -> Result<String, String> {
    parser
        .value()
        .map(|value| value.to_string_lossy().into_owned())
        .map_err(|error| format!("{name} 缺少参数: {error}"))
}