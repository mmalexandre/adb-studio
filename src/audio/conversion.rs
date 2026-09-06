use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
};

#[derive(Clone, Debug)]
pub struct ConversionJob {
    pub source: PathBuf,
    pub temporary: PathBuf,
    pub destination: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ConversionUpdate {
    pub index: usize,
    pub progress: f32,
    pub status: String,
}

pub fn collect_files(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_recursive(path, &mut files);
    files.sort();
    files
}

fn collect_files_recursive(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        if is_audio(path) {
            files.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_files_recursive(&entry.path(), files);
    }
}

pub fn is_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("flac" | "mp3" | "ogg" | "opus" | "wav")
    )
}

pub fn start(
    jobs: Vec<ConversionJob>,
    format: String,
    quality: String,
    sender: Sender<ConversionUpdate>,
    cancelled: Arc<AtomicBool>,
) {
    let next_job = Arc::new(AtomicUsize::new(0));
    let worker_count = jobs.len().min(4);
    for _ in 0..worker_count {
        let jobs = jobs.clone();
        let format = format.clone();
        let quality = quality.clone();
        let sender = sender.clone();
        let cancelled = Arc::clone(&cancelled);
        let next_job = Arc::clone(&next_job);
        thread::spawn(move || loop {
            if cancelled.load(Ordering::Acquire) {
                let index = next_job.fetch_add(1, Ordering::AcqRel);
                if jobs.get(index).is_some() {
                    let _ = sender.send(ConversionUpdate {
                        index,
                        progress: 0.0,
                        status: "Cancelled".to_string(),
                    });
                    continue;
                }
                break;
            }
            let index = next_job.fetch_add(1, Ordering::AcqRel);
            let Some(job) = jobs.get(index) else {
                break;
            };
            let result = convert(job, &format, &quality);
            let status = match result {
                Ok(()) => "Complete".to_string(),
                Err(error) => format!("Error: {error}"),
            };
            let progress = if status == "Complete" { 1.0 } else { 0.0 };
            let _ = sender.send(ConversionUpdate {
                index,
                progress,
                status,
            });
        });
    }
}

fn convert(job: &ConversionJob, format: &str, quality: &str) -> Result<(), String> {
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error", "-i"]);
    command.arg(&job.source);
    command.args(codec_args(format, quality));
    command.arg(&job.temporary);
    let status = command
        .status()
        .map_err(|error| format!("could not start ffmpeg: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg exited with {status}"))
    }
}

fn codec_args(format: &str, quality: &str) -> Vec<String> {
    match format {
        "mp3" => vec![
            "-c:a".into(),
            "libmp3lame".into(),
            "-b:a".into(),
            bitrate(quality, &["96k", "128k", "192k", "320k"]).into(),
        ],
        "ogg" => vec![
            "-c:a".into(),
            "libvorbis".into(),
            "-b:a".into(),
            bitrate(quality, &["96k", "128k", "192k", "256k"]).into(),
        ],
        "opus" => vec![
            "-c:a".into(),
            "libopus".into(),
            "-b:a".into(),
            bitrate(quality, &["64k", "96k", "128k", "192k"]).into(),
        ],
        "flac" => vec![
            "-c:a".into(),
            "flac".into(),
            "-compression_level".into(),
            compression_level(quality).into(),
        ],
        _ => vec![
            "-c:a".into(),
            match quality {
                "Low" => "pcm_s16le",
                "Medium" => "pcm_s24le",
                "High" => "pcm_s32le",
                _ => "pcm_f32le",
            }
            .into(),
        ],
    }
}

fn bitrate<'a>(quality: &str, values: &'a [&'a str; 4]) -> &'a str {
    match quality {
        "Low" => values[0],
        "Medium" => values[1],
        "High" => values[2],
        "Maximum" => values[3],
        _ => values[1],
    }
}

fn compression_level(quality: &str) -> &'static str {
    match quality {
        "Low" => "1",
        "Medium" => "5",
        "High" => "8",
        "Maximum" => "12",
        _ => "5",
    }
}
