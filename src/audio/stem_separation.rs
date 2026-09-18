use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::metadata::{self, StemExport, StemFile};

pub const MODEL: &str = "htdemucs";

pub fn is_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("flac" | "mp3" | "ogg" | "opus" | "wav")
    )
}

pub fn demucs_python() -> Option<PathBuf> {
    let root = dirs::data_dir()?.join("adb-studio").join("demucs");
    let python = if cfg!(windows) {
        root.join("venv").join("Scripts").join("python.exe")
    } else {
        root.join("venv").join("bin").join("python")
    };
    let valid = python.is_file()
        && Command::new(&python)
            .args(["-m", "demucs", "--help"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        && torch_backend_matches(&python);
    valid.then_some(python)
}

fn torch_backend_matches(python: &Path) -> bool {
    let output = Command::new(python)
        .args([
            "-c",
            "import torch, torchaudio; assert torchaudio.__version__.startswith('2.6.'); print('rocm' if torch.version.hip else 'cuda' if torch.version.cuda else 'cpu')",
        ])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let backend = String::from_utf8_lossy(&output.stdout);
    match requested_torch_backend() {
        "rocm" => backend.trim() == "rocm",
        _ => backend.trim() == "cpu",
    }
}

fn requested_torch_backend() -> &'static str {
    if cfg!(target_os = "linux")
        && std::env::var("ADB_STUDIO_TORCH_BACKEND")
            .ok()
            .is_some_and(|backend| backend.eq_ignore_ascii_case("rocm"))
        && command_available("rocminfo")
    {
        "rocm"
    } else {
        "cpu"
    }
}

#[derive(Clone, Debug)]
pub struct StemJob {
    pub workspace: PathBuf,
    pub source: PathBuf,
    pub output_folder: String,
    pub format: String,
    pub overwrite: bool,
}

#[derive(Debug)]
pub enum StemUpdate {
    Progress { progress: f32, status: String },
    Complete,
    Error(String),
    Cancelled,
}

pub fn normalized_output_folder(value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(PathBuf::new());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Stem output folder must be inside the workspace".into());
    }
    Ok(path.to_path_buf())
}

pub fn expected_output_directory(job: &StemJob) -> Result<PathBuf, String> {
    let folder = normalized_output_folder(&job.output_folder)?;
    let stem_name = job
        .source
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "Source file has no usable name".to_string())?;
    Ok(job.workspace.join(folder).join(stem_name))
}

pub fn is_exported(job: &StemJob) -> Result<bool, String> {
    let parent_hash = metadata::checksum_for_file(&job.workspace, &job.source)
        .map_err(|error| format!("Could not hash source file: {error}"))?;
    let parent_path = relative_path(&job.workspace, &job.source)?;
    let output_folder = normalized_output_folder(&job.output_folder)?
        .to_string_lossy()
        .replace('\\', "/");
    let manifest = metadata::load_stem_manifest(&job.workspace);
    let Some(export) = manifest.exports.iter().find(|export| {
        export.parent_path == parent_path
            && export.parent_hash == parent_hash
            && export.model == MODEL
            && export.format == job.format
            && export.output_folder == output_folder
    }) else {
        return Ok(false);
    };
    Ok(export.stems.iter().all(|stem| {
        let path = job.workspace.join(&stem.path);
        path.is_file()
            && metadata::checksum_for_file(&job.workspace, &path)
                .map(|hash| hash == stem.hash)
                .unwrap_or(false)
    }))
}

pub fn start(job: StemJob, sender: Sender<StemUpdate>, cancelled: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let result = run(job, &sender, &cancelled);
        match result {
            Ok(()) => {
                let _ = sender.send(StemUpdate::Complete);
            }
            Err(error) if error == "cancelled" => {
                let _ = sender.send(StemUpdate::Cancelled);
            }
            Err(error) => {
                let _ = sender.send(StemUpdate::Error(error));
            }
        }
    });
}

fn run(job: StemJob, sender: &Sender<StemUpdate>, cancelled: &AtomicBool) -> Result<(), String> {
    if !job.source.is_file() {
        return Err("Source audio file does not exist".into());
    }
    let output_directory = expected_output_directory(&job)?;
    if output_directory.exists() {
        if !job.overwrite {
            return Err("Stems already exist at the configured output location".into());
        }
        fs::remove_dir_all(&output_directory)
            .map_err(|error| format!("Could not remove existing stems: {error}"))?;
    }
    let temporary = job
        .workspace
        .join(".adbstudio")
        .join(format!("stems-{}", std::process::id()));
    fs::create_dir_all(&temporary)
        .map_err(|error| format!("Could not create temporary directory: {error}"))?;
    let result = run_inner(&job, &output_directory, &temporary, sender, cancelled);
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
        let _ = fs::remove_dir_all(&output_directory);
    }
    result
}

fn run_inner(
    job: &StemJob,
    output_directory: &Path,
    temporary: &Path,
    sender: &Sender<StemUpdate>,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let python = ensure_demucs(sender, cancelled)?;
    check_cancelled(cancelled)?;
    send_progress(sender, 0.35, "Separating audio with Demucs");
    let mut process = Command::new(&python)
        .args(["-m", "demucs.separate", "-n", MODEL, "--out"])
        .arg(temporary)
        .arg(&job.source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start Demucs: {error}"))?;
    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| "Could not read Demucs output".to_string())?;
    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| "Could not read Demucs errors".to_string())?;
    let stdout_thread = std::thread::spawn({
        let sender = sender.clone();
        move || relay_demucs_progress(stdout, sender)
    });
    let stderr_thread = std::thread::spawn({
        let sender = sender.clone();
        move || relay_demucs_progress(stderr, sender)
    });
    let status = process
        .wait()
        .map_err(|error| format!("Could not wait for Demucs: {error}"))?;
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    if !status.success() {
        return Err(format!("Demucs exited with {status}"));
    }
    check_cancelled(cancelled)?;

    let source_stem = job
        .source
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("track");
    let demucs_directory = temporary.join(MODEL).join(source_stem);
    let stem_names = ["vocals", "drums", "bass", "other"];
    fs::create_dir_all(output_directory)
        .map_err(|error| format!("Could not create output directory: {error}"))?;
    let mut stems = Vec::new();
    for (index, kind) in stem_names.iter().enumerate() {
        check_cancelled(cancelled)?;
        let source = demucs_directory.join(format!("{kind}.wav"));
        if !source.is_file() {
            return Err(format!("Demucs did not produce {kind}.wav"));
        }
        send_progress(
            sender,
            0.55 + index as f32 * 0.09,
            &format!("Exporting {kind} stem"),
        );
        let destination = output_directory.join(format!("{kind}.{}", job.format));
        let status = Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args(codec_args(&job.format))
            .arg(&destination)
            .status()
            .map_err(|error| format!("Could not start ffmpeg: {error}"))?;
        if !status.success() {
            return Err(format!("ffmpeg exited with {status}"));
        }
        stems.push((kind.to_string(), destination));
    }

    let parent_hash = metadata::checksum_for_file(&job.workspace, &job.source)
        .map_err(|error| format!("Could not hash source file: {error}"))?;
    let parent_path = relative_path(&job.workspace, &job.source)?;
    let output_folder = normalized_output_folder(&job.output_folder)?
        .to_string_lossy()
        .replace('\\', "/");
    let manifest_stems = stems
        .into_iter()
        .map(|(kind, path)| {
            Ok(StemFile {
                kind,
                path: relative_path(&job.workspace, &path)?,
                hash: metadata::checksum_for_file(&job.workspace, &path)
                    .map_err(|error| format!("Could not hash generated stem: {error}"))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut manifest = metadata::load_stem_manifest(&job.workspace);
    manifest
        .exports
        .retain(|export| !(export.parent_path == parent_path && export.parent_hash == parent_hash));
    manifest.exports.push(StemExport {
        parent_path,
        parent_hash,
        model: MODEL.into(),
        format: job.format.clone(),
        output_folder,
        stems: manifest_stems,
        exported_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs().to_string())
            .unwrap_or_default(),
    });
    metadata::save_stem_manifest(&job.workspace, &manifest)
        .map_err(|error| format!("Could not save stem manifest: {error}"))?;
    let _ = fs::remove_dir_all(temporary);
    send_progress(sender, 1.0, "Stem export complete");
    Ok(())
}

fn codec_args(format: &str) -> Vec<&'static str> {
    match format {
        "wav" => vec!["-c:a", "pcm_s24le"],
        "mp3" => vec!["-c:a", "libmp3lame", "-b:a", "320k"],
        _ => vec!["-c:a", "flac"],
    }
}

fn relay_demucs_progress<R: Read>(mut reader: R, sender: Sender<StemUpdate>) {
    let mut buffer = [0u8; 4096];
    let mut line = String::new();
    loop {
        let Ok(count) = reader.read(&mut buffer) else {
            break;
        };
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            if *byte == b'\r' || *byte == b'\n' {
                send_demucs_progress(&sender, &line);
                line.clear();
            } else {
                line.push(*byte as char);
            }
        }
    }
    send_demucs_progress(&sender, &line);
}

fn send_demucs_progress(sender: &Sender<StemUpdate>, output: &str) {
    let Some(percent) = output
        .split('%')
        .next()
        .and_then(|value| value.split_whitespace().last())
        .and_then(|value| value.parse::<f32>().ok())
    else {
        return;
    };
    let progress = 0.35 + (percent.clamp(0.0, 100.0) / 100.0) * 0.2;
    send_progress(
        sender,
        progress,
        &format!("Separating audio with Demucs ({percent:.0}%)"),
    );
}

fn ensure_demucs(sender: &Sender<StemUpdate>, cancelled: &AtomicBool) -> Result<PathBuf, String> {
    let root = dirs::data_dir()
        .ok_or_else(|| "Could not determine application data directory".to_string())?
        .join("adb-studio")
        .join("demucs");
    if let Some(python) = demucs_python() {
        return Ok(python);
    }
    let python = if cfg!(windows) {
        root.join("venv").join("Scripts").join("python.exe")
    } else {
        root.join("venv").join("bin").join("python")
    };
    send_progress(sender, 0.05, "Creating Demucs environment");
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create Demucs directory: {error}"))?;
    let system_python = if cfg!(windows) { "python" } else { "python3" };
    let status = Command::new(system_python)
        .args(["-m", "venv"])
        .arg(root.join("venv"))
        .status()
        .map_err(|error| format!("Python is required to install Demucs: {error}"))?;
    if !status.success() {
        return Err(format!(
            "{system_python} could not create a virtual environment"
        ));
    }
    check_cancelled(cancelled)?;
    send_progress(sender, 0.2, "Downloading PyTorch for the detected backend");
    let torch_index = pytorch_index_url();
    let status = Command::new(&python)
        .args([
            "-m",
            "pip",
            "install",
            "--force-reinstall",
            "torch==2.6.0",
            "torchaudio==2.6.0",
            "--index-url",
        ])
        .arg(torch_index)
        .status()
        .map_err(|error| format!("Could not start pip: {error}"))?;
    if !status.success() {
        return Err(format!(
            "pip could not install the selected PyTorch backend ({status})"
        ));
    }
    check_cancelled(cancelled)?;
    send_progress(sender, 0.27, "Downloading Demucs");
    let status = Command::new(&python)
        .args(["-m", "pip", "install", "--no-deps", "demucs==4.0.1"])
        .status()
        .map_err(|error| format!("Could not start pip: {error}"))?;
    if !status.success() {
        return Err(format!("pip exited with {status}"));
    }
    let status = Command::new(&python)
        .args([
            "-m",
            "pip",
            "install",
            "dora-search",
            "einops",
            "julius",
            "lameenc",
            "openunmix",
            "pyyaml",
            "retrying",
            "soundfile",
            "submitit",
            "tqdm",
        ])
        .status()
        .map_err(|error| format!("Could not start pip: {error}"))?;
    if !status.success() {
        return Err(format!(
            "pip could not install Demucs dependencies ({status})"
        ));
    }
    check_cancelled(cancelled)?;
    send_progress(sender, 0.3, "Verifying Demucs installation");
    Ok(python)
}

fn pytorch_index_url() -> &'static str {
    if requested_torch_backend() == "rocm" {
        "https://download.pytorch.org/whl/rocm6.3"
    } else {
        "https://download.pytorch.org/whl/cpu"
    }
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn send_progress(sender: &Sender<StemUpdate>, progress: f32, status: &str) {
    let _ = sender.send(StemUpdate::Progress {
        progress,
        status: status.into(),
    });
}

fn relative_path(workspace: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(workspace)
        .map_err(|_| "Generated path is outside the workspace".to_string())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::{expected_output_directory, normalized_output_folder, StemJob};
    use std::path::PathBuf;

    #[test]
    fn output_folder_is_workspace_relative() {
        assert_eq!(normalized_output_folder("").unwrap(), PathBuf::new());
        assert!(normalized_output_folder("../outside").is_err());
        assert!(normalized_output_folder("/outside").is_err());
        let job = StemJob {
            workspace: PathBuf::from("workspace"),
            source: PathBuf::from("workspace/song.wav"),
            output_folder: "stems".into(),
            format: "flac".into(),
            overwrite: false,
        };
        assert_eq!(
            expected_output_directory(&job).unwrap(),
            PathBuf::from("workspace/stems/song")
        );
    }
}
