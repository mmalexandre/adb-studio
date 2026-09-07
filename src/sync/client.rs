use reqwest::blocking::Client;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc::Sender,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::metadata;

use super::{RemoteFile, SyncConfig, SyncError, WorkflowRunUpdate};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const API_POLL_INTERVAL: Duration = Duration::from_millis(500);
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct ComfyUiClient {
    client: Client,
}

impl ComfyUiClient {
    pub fn new() -> Result<Self, SyncError> {
        Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("adb-studio/0.1")
            .build()
            .map(|client| Self { client })
            .map_err(SyncError::Request)
    }

    pub fn list_files(&self, config: &SyncConfig) -> Result<Vec<RemoteFile>, SyncError> {
        validate_request_config(config)?;
        let response = self
            .client
            .get(format!("{}/adb-music-player/audio-files", config.url))
            .query(&[("directory", config.remote_directory.as_str())])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(SyncError::Request)?
            .error_for_status()
            .map_err(SyncError::Request)?;
        let mut files: Vec<RemoteFile> = response.json().map_err(SyncError::Request)?;
        if files
            .iter()
            .any(|file| file.name.trim().is_empty() || file.path.trim().is_empty())
        {
            return Err(SyncError::Response(
                "ComfyUI returned a file without a name or path".to_string(),
            ));
        }
        sort_remote_files(&mut files);
        Ok(files)
    }

    pub fn upload_workflow(
        &self,
        config: &SyncConfig,
        workflow: &serde_json::Value,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        let response = self
            .client
            .post(format!("{}/adb-music-player/workflow", config.url))
            .json(workflow)
            .send()
            .map_err(SyncError::Request)?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            let detail = body.trim();
            return Err(SyncError::Response(if detail.is_empty() {
                format!("ComfyUI rejected workflow ({status})")
            } else {
                format!("ComfyUI rejected workflow ({status}): {detail}")
            }));
        }
        Ok(())
    }

    pub fn run_workflow(
        &self,
        config: &SyncConfig,
        workflow: &serde_json::Value,
        workspace: &Path,
        output_directory: &Path,
        output_stem: &str,
        cancelled: &std::sync::atomic::AtomicBool,
        updates: &Sender<WorkflowRunUpdate>,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        fs::create_dir_all(output_directory).map_err(SyncError::Io)?;
        let client_id = format!(
            "adb-studio-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let prompt_response = self
            .client
            .post(format!("{}/prompt", config.url))
            .json(&serde_json::json!({ "prompt": workflow, "client_id": client_id }))
            .send()
            .map_err(SyncError::Request)?;
        if !prompt_response.status().is_success() {
            return Err(response_error(prompt_response, "ComfyUI rejected prompt"));
        }
        let prompt: serde_json::Value = prompt_response.json().map_err(SyncError::Request)?;
        let prompt_id = prompt
            .get("prompt_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SyncError::Response("ComfyUI did not return a prompt id".into()))?
            .to_string();

        let history = loop {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                self.interrupt(config)?;
                let _ = updates.send(WorkflowRunUpdate::Cancelled);
                return Ok(());
            }
            if let Some((progress, step)) = self.progress(config)? {
                let _ = updates.send(WorkflowRunUpdate::Progress { progress, step });
            } else {
                let _ = updates.send(WorkflowRunUpdate::Progress {
                    progress: 0.05,
                    step: "Waiting for ComfyUI".into(),
                });
            }
            let response = self
                .client
                .get(format!("{}/history/{}", config.url, prompt_id))
                .send()
                .map_err(SyncError::Request)?;
            if response.status().is_success() {
                let value: serde_json::Value = response.json().map_err(SyncError::Request)?;
                if value.get(&prompt_id).is_some() {
                    break value;
                }
            }
            thread::sleep(API_POLL_INTERVAL);
        };

        let output = find_audio_output(
            history
                .get(&prompt_id)
                .and_then(|entry| entry.get("outputs"))
                .ok_or_else(|| SyncError::Response("ComfyUI returned no outputs".into()))?,
        )
        .ok_or_else(|| SyncError::Response("ComfyUI returned no audio output".into()))?;
        let audio_path =
            output_directory.join(format!("{output_stem}.{}", output_file_extension(&output)));
        self.download_view(config, &output, &audio_path)?;
        let workflow_path = metadata::workflow_path(workspace, &audio_path)
            .ok_or_else(|| SyncError::Response("Output path is outside the workspace".into()))?;
        if let Some(parent) = workflow_path.parent() {
            fs::create_dir_all(parent).map_err(SyncError::Io)?;
        }
        let contents = serde_json::to_string_pretty(workflow)
            .map_err(|error| SyncError::Response(error.to_string()))?;
        fs::write(&workflow_path, contents).map_err(SyncError::Io)?;
        let _ = updates.send(WorkflowRunUpdate::Finished {
            audio_path,
            workflow_path,
        });
        Ok(())
    }

    fn interrupt(&self, config: &SyncConfig) -> Result<(), SyncError> {
        let response = self
            .client
            .post(format!("{}/interrupt", config.url))
            .send()
            .map_err(SyncError::Request)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(
                response,
                "ComfyUI could not cancel the prompt",
            ))
        }
    }

    fn progress(&self, config: &SyncConfig) -> Result<Option<(f32, String)>, SyncError> {
        let response = self
            .client
            .get(format!("{}/progress", config.url))
            .send()
            .map_err(SyncError::Request)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let value: serde_json::Value = response.json().map_err(SyncError::Request)?;
        let max = value
            .get("max")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0)
            .max(1.0);
        let current = value
            .get("value")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let step = value
            .get("node")
            .and_then(serde_json::Value::as_str)
            .map(|node| format!("Running node {node}"))
            .unwrap_or_else(|| "Running workflow".into());
        Ok(Some(((current / max).clamp(0.0, 1.0) as f32, step)))
    }

    fn download_view(
        &self,
        config: &SyncConfig,
        output: &AudioOutput,
        destination: &Path,
    ) -> Result<(), SyncError> {
        let filename = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("output.audio");
        let source_extension = Path::new(&output.filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("audio")
            .to_ascii_lowercase();
        let temporary_directory = destination.parent().unwrap_or_else(|| Path::new("."));
        let source_temporary =
            temporary_download_path(temporary_directory, &format!("source.{source_extension}"));
        let output_temporary = temporary_download_path(temporary_directory, filename);
        let result = (|| {
            let mut response = self
                .client
                .get(format!("{}/view", config.url))
                .query(&[
                    ("filename", output.filename.as_str()),
                    ("subfolder", output.subfolder.as_str()),
                    ("type", output.output_type.as_str()),
                ])
                .send()
                .map_err(SyncError::Request)?
                .error_for_status()
                .map_err(SyncError::Request)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&source_temporary)
                .map_err(SyncError::Io)?;
            response.copy_to(&mut file).map_err(SyncError::Request)?;
            file.flush().map_err(SyncError::Io)?;
            file.sync_all().map_err(SyncError::Io)?;
            if !source_extension.eq_ignore_ascii_case("wav") {
                fs::rename(&source_temporary, destination).map_err(SyncError::Io)?;
            } else {
                let status = Command::new("ffmpeg")
                    .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
                    .arg(&source_temporary)
                    .args(["-c:a", "flac"])
                    .arg(&output_temporary)
                    .status()
                    .map_err(|error| {
                        SyncError::Response(format!("could not start ffmpeg: {error}"))
                    })?;
                if !status.success() {
                    return Err(SyncError::Response(format!("ffmpeg exited with {status}")));
                }
                fs::rename(&output_temporary, destination).map_err(SyncError::Io)?;
                fs::remove_file(&source_temporary).map_err(SyncError::Io)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&source_temporary);
            let _ = fs::remove_file(&output_temporary);
        }
        result
    }

    pub fn download_file(
        &self,
        config: &SyncConfig,
        file: &RemoteFile,
        destination: &Path,
    ) -> Result<(), SyncError> {
        validate_request_config(config)?;
        let filename = Path::new(&file.name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| {
                SyncError::Response("ComfyUI returned an invalid filename".to_string())
            })?;
        fs::create_dir_all(destination).map_err(SyncError::Io)?;
        let final_path = destination.join(filename);
        let temporary_path = temporary_download_path(destination, filename);
        let result = (|| {
            let mut response = self
                .client
                .get(format!("{}/adb-music-player/audio-download", config.url))
                .query(&[("path", file.path.as_str())])
                .send()
                .map_err(SyncError::Request)?
                .error_for_status()
                .map_err(SyncError::Request)?;
            let mut temporary_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(SyncError::Io)?;
            response
                .copy_to(&mut temporary_file)
                .map_err(SyncError::Request)?;
            temporary_file.flush().map_err(SyncError::Io)?;
            temporary_file.sync_all().map_err(SyncError::Io)?;
            fs::rename(&temporary_path, &final_path).map_err(SyncError::Io)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }

    pub(super) fn download_optional_file(
        &self,
        config: &SyncConfig,
        file: &RemoteFile,
        destination: &Path,
    ) -> Result<bool, SyncError> {
        validate_request_config(config)?;
        let filename = destination
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| SyncError::Response("Invalid workflow filename".to_string()))?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(SyncError::Io)?;
        }
        let temporary_path = temporary_download_path(
            destination.parent().unwrap_or_else(|| Path::new(".")),
            filename,
        );
        let result = (|| {
            let response = self
                .client
                .get(format!("{}/adb-music-player/audio-download", config.url))
                .query(&[("path", file.path.as_str())])
                .send()
                .map_err(SyncError::Request)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(false);
            }
            let mut response = response.error_for_status().map_err(SyncError::Request)?;
            let mut temporary_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(SyncError::Io)?;
            response
                .copy_to(&mut temporary_file)
                .map_err(SyncError::Request)?;
            temporary_file.flush().map_err(SyncError::Io)?;
            temporary_file.sync_all().map_err(SyncError::Io)?;
            fs::rename(&temporary_path, destination).map_err(SyncError::Io)?;
            Ok(true)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }
}

#[derive(Clone, Debug)]
struct AudioOutput {
    filename: String,
    subfolder: String,
    output_type: String,
}

fn output_file_extension(output: &AudioOutput) -> String {
    let extension = Path::new(&output.filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("audio");
    if extension.eq_ignore_ascii_case("wav") {
        "flac".into()
    } else {
        extension.to_ascii_lowercase()
    }
}

fn find_audio_output(value: &serde_json::Value) -> Option<AudioOutput> {
    if let Some(object) = value.as_object() {
        if let Some(filename) = object.get("filename").and_then(serde_json::Value::as_str) {
            let extension = Path::new(filename)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase());
            if matches!(
                extension.as_deref(),
                Some("flac" | "wav" | "mp3" | "ogg" | "opus")
            ) {
                return Some(AudioOutput {
                    filename: filename.into(),
                    subfolder: object
                        .get("subfolder")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    output_type: object
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("output")
                        .into(),
                });
            }
        }
        for child in object.values() {
            if let Some(output) = find_audio_output(child) {
                return Some(output);
            }
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            if let Some(output) = find_audio_output(child) {
                return Some(output);
            }
        }
    }
    None
}

fn response_error(response: reqwest::blocking::Response, prefix: &str) -> SyncError {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = body.trim();
    SyncError::Response(if detail.is_empty() {
        format!("{prefix} ({status})")
    } else {
        format!("{prefix} ({status}): {detail}")
    })
}

fn temporary_download_path(destination: &Path, filename: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    destination.join(format!(
        ".{filename}.{}.{timestamp}.{counter}",
        std::process::id()
    ))
}

fn sort_remote_files(files: &mut [RemoteFile]) {
    files.sort_by(|left, right| right.modified.cmp(&left.modified));
}

fn validate_request_config(config: &SyncConfig) -> Result<(), SyncError> {
    if config.url.is_empty() {
        return Err(SyncError::InvalidConfig(
            "ComfyUI URL is required".to_string(),
        ));
    }
    if !(config.url.starts_with("http://") || config.url.starts_with("https://")) {
        return Err(SyncError::InvalidConfig(
            "ComfyUI URL must start with http:// or https://".to_string(),
        ));
    }
    if config.remote_directory.is_empty() {
        return Err(SyncError::InvalidConfig(
            "Remote directory is required".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{sort_remote_files, temporary_download_path};
    use crate::sync::RemoteFile;
    use std::path::Path;

    #[test]
    fn temporary_download_name_is_hidden_and_keeps_original_name() {
        let path = temporary_download_path(Path::new("/workspace/downloads"), "myfile.opus");
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with(".myfile.opus."));
        assert_ne!(filename, "myfile.opus");
    }

    #[test]
    fn remote_files_are_sorted_newest_first() {
        let mut files = vec![
            RemoteFile {
                name: "older.mp3".to_string(),
                path: "/remote/older.mp3".to_string(),
                modified: 1,
            },
            RemoteFile {
                name: "newer.mp3".to_string(),
                path: "/remote/newer.mp3".to_string(),
                modified: 2,
            },
        ];
        sort_remote_files(&mut files);
        assert_eq!(files[0].name, "newer.mp3");
        assert_eq!(files[1].name, "older.mp3");
    }
}
