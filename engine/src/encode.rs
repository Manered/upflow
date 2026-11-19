use crate::{
    media::{encoder_time_base, ffmpeg_error, null_device, probe, LOCAL_FORMATS},
    mp4, process, Error, Media, Settings, Tools,
};
use serde::Serialize;
use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};
use tempfile::{NamedTempFile, TempPath};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tokio_util::sync::CancellationToken;

const BITRATE_KBPS: u64 = 16_617;

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    Progress {
        pass: u8,
        percent: f64,
        pass_percent: f64,
        elapsed: f64,
        remaining: Option<f64>,
    },
    Log {
        line: String,
    },
    Finalizing,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub directory: PathBuf,
}

pub struct Conversion {
    pub source: PathBuf,
    pub directory: Option<PathBuf>,
    pub settings: Settings,
    pub log_directory: PathBuf,
    pub tools: Tools,
}

pub fn plan_outputs(sources: &[PathBuf], settings: &Settings) -> Vec<Option<PathBuf>> {
    let mut reserved = HashSet::new();
    sources
        .iter()
        .map(|source| {
            let directory = output_directory(source, None, settings)?;
            (1..=10_000).find_map(|number| {
                let path = output_path(source, directory, number);
                #[cfg(windows)]
                let key = path.to_string_lossy().to_lowercase();
                #[cfg(not(windows))]
                let key = path.clone();
                (path.symlink_metadata().is_err() && reserved.insert(key)).then_some(path)
            })
        })
        .collect()
}

fn output_directory<'a>(
    source: &'a Path,
    requested: Option<&'a Path>,
    settings: &'a Settings,
) -> Option<&'a Path> {
    let configured = if settings.use_source_directory {
        None
    } else {
        settings.output_directory.as_deref()
    };
    requested.or(configured).or_else(|| source.parent())
}

type Reporter<'a> = dyn Fn(Event) + Send + Sync + 'a;

pub async fn convert(
    job: Conversion,
    cancel: CancellationToken,
    report: impl Fn(Event) + Send + Sync,
) -> Result<Output, Error> {
    let started = Instant::now();
    let log = if job.settings.keep_logs {
        fs::create_dir_all(&job.log_directory)
            .map_err(|e| Error::io(e, "logs", "Could not create the logs folder."))?;
        let log = tempfile::Builder::new()
            .prefix("upflow-")
            .suffix(".log")
            .tempfile_in(&job.log_directory)
            .and_then(|f| f.keep().map_err(|e| e.error))
            .map_err(|e| Error::io(e, "logs", "Could not create the conversion log."))?;
        report(Event::Log {
            line: format!("Log file: {}", log.1.display()),
        });
        Some(log.0)
    } else {
        None
    };
    let mut log = Log {
        report: &report,
        file: log,
        tail: VecDeque::new(),
    };
    let result = encode(job, &cancel, &mut log, started).await;
    if let Err(error) = &result {
        log.line(
            format!("{} {}", error.message, error.detail)
                .trim()
                .to_owned(),
        );
    }
    result
}

async fn encode(
    job: Conversion,
    cancel: &CancellationToken,
    log: &mut Log<'_>,
    started: Instant,
) -> Result<Output, Error> {
    check_cancel(cancel)?;
    let media = probe(&job.source, &job.tools, cancel).await?;
    let level = level_for(&media)?;
    let time_base = encoder_time_base(&media, &job.tools, cancel).await?;
    let directory = output_directory(&job.source, job.directory.as_deref(), &job.settings)
        .expect("absolute video path has a parent")
        .to_owned();
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(Error::new(
            "outputDirectory",
            "Choose an existing output folder.",
            directory.display(),
        ));
    }
    let work = tempfile::Builder::new()
        .prefix(".upflow-")
        .tempdir_in(&directory)
        .map_err(|e| {
            Error::io(
                e,
                "outputDirectory",
                "Could not write to the output folder.",
            )
        })?;
    // Reserve room for x264 stats, faststart and a nominal output. Very simple clips can consume less.
    let expected = (media.duration * ((BITRATE_KBPS + 160) * 125) as f64 * 1.08) as u64;
    if fs2::available_space(&directory).is_ok_and(|available| available < expected) {
        return Err(Error::new(
            "diskFull",
            "There is not enough disk space for the estimated output.",
            "",
        ));
    }
    let passlog = work.path().join("x264");
    let temp = NamedTempFile::new_in(work.path())
        .map_err(|e| {
            Error::io(
                e,
                "outputDirectory",
                "Could not create the temporary output.",
            )
        })?
        .into_temp_path();
    let temp_path: &Path = temp.as_ref();

    let mut previous_frames = 0;
    let mut previous_speed = None;
    for pass in [1, 2] {
        check_cancel(cancel)?;
        let mut command = pass_command(
            &job.tools,
            &media,
            &job.settings,
            (level, time_base),
            pass,
            &passlog,
            temp_path,
        );
        let mut progress = Progress::new(pass, media.duration, started);
        progress.speed = previous_speed;
        progress.emit(log.report, false);
        log.line(format!(
            "Pass {pass} of 2 | libx264 High {level} | {BITRATE_KBPS} kbps"
        ));
        log.line(format!("{command:?}"));
        run_pass(&mut command, &mut progress, log, cancel).await?;
        if progress.frame == 0 || (pass == 2 && previous_frames != progress.frame) {
            return Err(Error::new(
                "encoder",
                "The encoder produced an unexpected number of frames.",
                format!(
                    "Previous pass: {previous_frames}, current pass: {}",
                    progress.frame
                ),
            ));
        }
        previous_frames = progress.frame;
        previous_speed = progress.speed;
        progress.emit(log.report, true);
    }

    check_cancel(cancel)?;
    (log.report)(Event::Finalizing);
    let mut output = probe(temp_path, &job.tools, cancel).await.map_err(|e| {
        if e.code == "cancelled" {
            e
        } else {
            Error::new(
                "invalidOutput",
                "The completed video could not be verified.",
                format!("{} {}", e.message, e.detail),
            )
        }
    })?;
    validate(&media, &output, previous_frames, level)?;
    if output.sample_rate == Some(48_000) && output.channel_layout.as_deref() == Some("stereo") {
        check_cancel(cancel)?;
        let staged = temp_path.to_owned();
        let patched = tokio::task::spawn_blocking(move || mp4::patch_audio_tail(&staged))
            .await
            .map_err(|error| {
                Error::new(
                    "invalidOutput",
                    "Could not apply the MP4 audio patch.",
                    error,
                )
            })?
            .map_err(|error| {
                Error::io(
                    error,
                    "invalidOutput",
                    "Could not apply the MP4 audio patch.",
                )
            })?;
        if patched {
            output = probe(temp_path, &job.tools, cancel).await?;
            validate(&media, &output, previous_frames, level)?;
            log.line("Applied the four-packet MP4 audio tail patch.".into());
        }
    }
    check_cancel(cancel)?;
    let path = deliver(temp, &directory, &media.path)?;
    log.line(format!("Saved {}", path.display()));
    Ok(Output {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path,
        size: output.size,
        directory,
    })
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::cancelled())
    } else {
        Ok(())
    }
}

pub(crate) fn level_for(media: &Media) -> Result<&'static str, Error> {
    if !media.width.is_multiple_of(2) || !media.height.is_multiple_of(2) {
        return Err(Error::new(
            "unsupportedDimensions",
            "This video needs even width and height for H.264.",
            "",
        ));
    }
    let width = media.width.div_ceil(16) as u64;
    let height = media.height.div_ceil(16) as u64;
    let frame = width * height;
    let levels = [
        ("4.2", 8_704, 522_240),
        ("5.0", 22_080, 589_824),
        ("5.1", 36_864, 983_040),
        ("5.2", 36_864, 2_073_600),
        ("6.0", 139_264, 4_177_920),
        ("6.1", 139_264, 8_355_840),
        ("6.2", 139_264, 16_711_680),
    ];
    // All listed levels have room for at least two reference frames at MaxFS.
    // The per-dimension limit is sqrt(8 * MaxFS) macroblocks.
    levels
        .into_iter()
        .find(|&(_, max_frame, max_mbps)| {
            frame <= max_frame
                && width.max(height).pow(2) <= 8 * max_frame
                && frame as f64 * media.max_fps <= max_mbps as f64
        })
        .map(|(level, _, _)| level)
        .ok_or_else(|| {
            Error::new(
                "unsupportedDimensions",
                "This resolution and frame rate exceed H.264 limits.",
                "",
            )
        })
}

fn pass_command(
    tools: &Tools,
    media: &Media,
    settings: &Settings,
    (level, time_base): (&str, &str),
    pass: u8,
    passlog: &Path,
    output: &Path,
) -> Command {
    let mut command = process::command(&tools.ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            "-nostdin",
            "-y",
            "-xerror",
            "-filter_threads",
            "1",
            "-nostats",
            "-stats_period",
            "0.25",
            "-progress",
            "pipe:1",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            LOCAL_FORMATS,
            "-noautorotate",
            "-i",
        ])
        .arg(&media.path)
        .args(["-map", &format!("0:{}", media.video_index)]);
    if pass == 2 {
        if let Some(index) = media.audio_index {
            command.args([
                "-map",
                &format!("0:{index}"),
                "-c:a",
                "aac",
                "-profile:a",
                "aac_low",
                "-b:a",
                "160k",
            ]);
        }
    }
    command
        .args([
            "-map_chapters",
            "-1",
            "-c:v",
            "libx264",
            "-preset",
            "faster",
            "-b:v",
            &format!("{BITRATE_KBPS}k"),
            "-profile:v",
            "high",
            "-level:v",
            level,
            "-pix_fmt",
            "yuv420p",
            "-fps_mode:v",
            "passthrough",
            "-enc_time_base:v",
            time_base,
            "-g",
            "250",
            "-keyint_min",
            "25",
            "-sc_threshold",
            "40",
            "-refs",
            "2",
            "-bf",
            "3",
            "-x264-params",
            "b-adapt=1:b-pyramid=2:rc-lookahead=20:mbtree=1:aq-mode=1:aq-strength=1.0:psy-rd=1.0,0.0",
            "-threads:v",
            &settings.threads.to_string(),
            "-pass",
            &pass.to_string(),
            "-passlogfile",
        ])
        .arg(passlog);
    if pass == 1 {
        command.args(["-an", "-f", "null", null_device()]);
    } else {
        command
            .args(["-movflags", "+faststart", "-f", "mp4"])
            .arg(output);
    }
    command
}

fn validate(input: &Media, output: &Media, encoded_frames: u64, level: &str) -> Result<(), Error> {
    // A container may estimate VFR frame rate from the span between its first and last timestamps.
    let rate_tolerance = (input.fps * (2.0 / encoded_frames as f64).max(0.01)).max(0.02);
    let duration_tolerance = (2.0 / input.fps).max(0.12);
    if output.video_codec != "h264"
        || output.profile.as_deref() != Some("High")
        || output.level != level.replace('.', "").parse::<i32>().ok()
        || output.pixel_format.as_deref() != Some("yuv420p")
        || !output.container.split(',').any(|format| format == "mp4")
        || (output.width, output.height) != (input.width, input.height)
        || output.frames != Some(encoded_frames)
        || (input.fps - output.fps).abs() > rate_tolerance
        || ((input.has_video_duration || input.audio_index.is_none())
            && (input.duration - output.duration).abs() > duration_tolerance)
        || (input.audio_index.is_some()
            && (output.audio_codec.as_deref() != Some("aac")
                || output.audio_profile.as_deref() != Some("LC")))
        || (input.audio_index.is_none() && output.audio_index.is_some())
    {
        return Err(Error::new(
            "invalidOutput",
            "The completed video did not pass validation.",
            format!("Input: {input:?}\nOutput: {output:?}\nEncoded frames: {encoded_frames}"),
        ));
    }
    Ok(())
}

fn output_path(input: &Path, directory: &Path, attempt: u32) -> PathBuf {
    let stem = input
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("video"));
    let mut name = OsString::from(stem);
    name.push("_upflow");
    if attempt > 1 {
        name.push(format!("_{attempt}"));
    }
    name.push(".mp4");
    directory.join(name)
}

fn deliver(mut temp: TempPath, directory: &Path, input: &Path) -> Result<PathBuf, Error> {
    for attempt in 1..=10_000 {
        let path = output_path(input, directory, attempt);
        match temp.persist_noclobber(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => temp = error.path,
            Err(error) => {
                return Err(Error::io(
                    error.error,
                    "outputDirectory",
                    "Could not save the completed video.",
                ))
            }
        }
    }
    Err(Error::new(
        "outputDirectory",
        "Could not find an unused output filename.",
        directory.display(),
    ))
}

struct Log<'a> {
    report: &'a Reporter<'a>,
    file: Option<File>,
    tail: VecDeque<String>,
}

impl Log<'_> {
    fn line(&mut self, line: String) {
        if line.is_empty() {
            return;
        }
        if let Some(file) = &mut self.file {
            if writeln!(file, "{line}").is_err() {
                self.file = None;
                (self.report)(Event::Log {
                    line: "Could not continue writing the saved log.".into(),
                });
            }
        }
        if self.tail.len() == 20 {
            self.tail.pop_front();
        }
        self.tail.push_back(line.clone());
        (self.report)(Event::Log { line });
    }
}

async fn run_pass(
    command: &mut Command,
    progress: &mut Progress,
    log: &mut Log<'_>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    log.tail.clear();
    let (mut child, tree) = process::spawn(command.stdout(Stdio::piped()).stderr(Stdio::piped()))?;
    let mut stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut stderr = BufReader::new(child.stderr.take().expect("piped"));
    let mut err_line = Vec::new();
    let (mut stdout_open, mut stderr_open) = (true, true);
    let consume = async {
        while stdout_open || stderr_open {
            tokio::select! {
                result = stdout.next_line(), if stdout_open => {
                    match result {
                        Ok(Some(line)) => progress.read(&line, log.report),
                        Ok(None) => stdout_open = false,
                        Err(error) => return Err(Error::io(error, "process", "Could not read encoding progress.")),
                    }
                }
                result = stderr.read_until(b'\n', &mut err_line), if stderr_open => {
                    match result {
                        Ok(0) => stderr_open = false,
                        Ok(_) => {
                            log.line(String::from_utf8_lossy(&err_line).trim_end().to_owned());
                            err_line.clear();
                        }
                        Err(error) => return Err(Error::io(error, "process", "Could not read encoding logs.")),
                    }
                }
            }
        }
        child
            .wait()
            .await
            .map_err(|e| Error::io(e, "process", "The video process ended unexpectedly."))
    };
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::cancelled()),
        result = consume => result,
    };
    tree.close();
    if outcome.is_err() {
        let _ = child.kill().await;
    }
    let status = outcome?;
    if !status.success() {
        let detail = format!("{status}\n{}", self_contained_tail(&log.tail));
        let crashed = status.code().is_none_or(|code| code < 0);
        return Err(if crashed {
            Error::new("process", "The video process ended unexpectedly.", detail)
        } else {
            ffmpeg_error(
                &detail,
                "encoder",
                "FFmpeg could not finish encoding this video.",
            )
        });
    }
    Ok(())
}

fn self_contained_tail(tail: &VecDeque<String>) -> String {
    tail.iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

struct Progress {
    pass: u8,
    duration: f64,
    started: Instant,
    pass_started: Instant,
    position: f64,
    frame: u64,
    speed: Option<f64>,
}

impl Progress {
    fn new(pass: u8, duration: f64, started: Instant) -> Self {
        Self {
            pass,
            duration,
            started,
            pass_started: Instant::now(),
            position: 0.0,
            frame: 0,
            speed: None,
        }
    }

    fn read(&mut self, line: &str, report: &Reporter<'_>) {
        let Some((key, value)) = line.split_once('=') else {
            return;
        };
        match key {
            "frame" => self.frame = value.trim().parse().unwrap_or(self.frame),
            "out_time_us" => {
                if let Ok(us) = value.parse::<i64>() {
                    self.position = self
                        .position
                        .max((us as f64 / 1_000_000.0).min(self.duration));
                }
            }
            "progress" => {
                let elapsed = self.pass_started.elapsed().as_secs_f64();
                if elapsed >= 1.5 && self.position / self.duration >= 0.03 {
                    let measured = self.position / elapsed;
                    self.speed = Some(
                        self.speed
                            .map_or(measured, |speed| speed * 0.75 + measured * 0.25),
                    );
                }
                self.emit(report, false);
            }
            _ => {}
        }
    }

    fn emit(&self, report: &Reporter<'_>, done: bool) {
        let part = if done {
            1.0
        } else {
            (self.position / self.duration).clamp(0.0, 0.998)
        };
        // Include the second pass while estimating time during the first.
        let pending = self.duration * (1.0 - part + f64::from(2 - self.pass));
        report(Event::Progress {
            pass: self.pass,
            percent: ((f64::from(self.pass - 1) + part) * 50.0).min(99.9),
            pass_percent: part * 100.0,
            elapsed: self.started.elapsed().as_secs_f64(),
            remaining: self
                .speed
                .filter(|speed| speed.is_finite() && *speed > 0.0)
                .map(|speed| (pending / speed).min(359_999.0)),
        });
    }
}
