use std::{
    collections::HashSet,
    fs,
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use crate::metadata;

use super::{
    ComfyUiClient, DownloadIndex, RemoteFile, SyncCommand, SyncConfig, SyncError, SyncEvent,
    SyncProgress,
};

pub(super) fn sync_loop(
    workspace: PathBuf,
    config: SyncConfig,
    command_receiver: Receiver<SyncCommand>,
    event_sender: Sender<SyncEvent>,
    generation: u64,
) {
    let client = match ComfyUiClient::new() {
        Ok(client) => client,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };
    let destination = match config.local_path(&workspace) {
        Ok(path) => path,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };
    if let Err(error) = fs::create_dir_all(&destination) {
        let _ = event_sender.send(SyncEvent::Error {
            generation,
            message: SyncError::Io(error).to_string(),
        });
        return;
    }
    let interval = Duration::from_millis(config.interval_ms.max(100));
    let mut download_index = match load_download_index(&workspace) {
        Ok(index) => index,
        Err(error) => {
            let _ = event_sender.send(SyncEvent::Error {
                generation,
                message: error.to_string(),
            });
            return;
        }
    };

    loop {
        let _ = event_sender.send(SyncEvent::Running { generation });
        let files = match client.list_files(&config) {
            Ok(files) => files,
            Err(error) => {
                let _ = event_sender.send(SyncEvent::Error {
                    generation,
                    message: error.to_string(),
                });
                return;
            }
        };
        let mut current = match progress_with_index(&files, &destination, &config, &download_index)
        {
            Ok(progress) => progress,
            Err(error) => {
                let _ = event_sender.send(SyncEvent::Error {
                    generation,
                    message: error.to_string(),
                });
                return;
            }
        };
        let _ = event_sender.send(SyncEvent::Progress {
            generation,
            progress: current.clone(),
        });

        let mut last_downloaded_path = None;
        for file in &files {
            if command_receiver.try_recv().is_ok() {
                return;
            }
            let filename = Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if filename.is_empty() {
                continue;
            }
            if !destination.join(filename).exists()
                && !download_index.contains_completed(&config, file)
            {
                if let Err(error) = client.download_file(&config, file, &destination) {
                    let _ = event_sender.send(SyncEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    return;
                }
                let size = match fs::metadata(destination.join(filename)) {
                    Ok(metadata) => metadata.len(),
                    Err(error) => {
                        let _ = event_sender.send(SyncEvent::Error {
                            generation,
                            message: SyncError::Io(error).to_string(),
                        });
                        return;
                    }
                };
                download_index.record_completed(&config, file, size);
                if let Err(error) = save_download_index(&workspace, &download_index) {
                    let _ = event_sender.send(SyncEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    return;
                }
                current.present += 1;
                let _ = event_sender.send(SyncEvent::Progress {
                    generation,
                    progress: current.clone(),
                });
                last_downloaded_path = Some(destination.join(filename));
            }
            match sync_workflow(&client, &config, file, &workspace, filename) {
                Ok(true) => {
                    let _ = event_sender.send(SyncEvent::WorkflowUpdated {
                        generation,
                        audio_path: destination.join(filename).to_string_lossy().into_owned(),
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    let _ = event_sender.send(SyncEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    return;
                }
            }
        }

        if let Some(audio_path) = last_downloaded_path {
            let _ = event_sender.send(SyncEvent::Downloaded {
                generation,
                audio_path: audio_path.to_string_lossy().into_owned(),
            });
        }

        match command_receiver.recv_timeout(interval) {
            Ok(SyncCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn sync_workflow(
    client: &ComfyUiClient,
    config: &SyncConfig,
    audio_file: &RemoteFile,
    workspace: &Path,
    audio_filename: &str,
) -> Result<bool, SyncError> {
    let workflow_path = workflow_local_path(workspace, audio_filename);
    if !workflow_path.exists() {
        let workflow_file = RemoteFile {
            name: format!("{}.workflow.json", audio_file.name),
            path: format!("{}.workflow.json", audio_file.path),
            modified: audio_file.modified,
        };
        if client.download_optional_file(config, &workflow_file, &workflow_path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn workflow_local_path(workspace: &Path, audio_filename: &str) -> PathBuf {
    metadata::workflow_path(workspace, &workspace.join(audio_filename))
        .expect("audio filename must be inside workspace")
}

fn load_download_index(workspace: &Path) -> Result<DownloadIndex, SyncError> {
    match fs::read_to_string(super::download_index_path(workspace)) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|error| SyncError::Response(error.to_string()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DownloadIndex::default()),
        Err(error) => Err(SyncError::Io(error)),
    }
}

fn save_download_index(workspace: &Path, index: &DownloadIndex) -> Result<(), SyncError> {
    fs::create_dir_all(workspace.join(".adbstudio")).map_err(SyncError::Io)?;
    let contents = serde_json::to_string_pretty(index)
        .map_err(|error| SyncError::Response(error.to_string()))?;
    fs::write(super::download_index_path(workspace), contents).map_err(SyncError::Io)
}

fn progress_with_index(
    files: &[RemoteFile],
    destination: &Path,
    config: &SyncConfig,
    download_index: &DownloadIndex,
) -> Result<SyncProgress, SyncError> {
    let local_names: HashSet<String> = match fs::read_dir(destination) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => HashSet::new(),
        Err(error) => return Err(SyncError::Io(error)),
    };
    let present = files
        .iter()
        .filter_map(|file| {
            Path::new(&file.name)
                .file_name()
                .and_then(|name| name.to_str())
        })
        .filter(|name| local_names.contains(*name))
        .count();
    let recorded = files
        .iter()
        .filter(|file| download_index.contains_completed(config, file))
        .count();
    Ok(SyncProgress {
        present: present.max(recorded),
        total: files.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::{load_download_index, progress_with_index, save_download_index, workflow_local_path};
    use crate::metadata;
    use crate::sync::{DownloadIndex, RemoteFile, SyncConfig};
    use std::{fs, path::Path, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};

    #[test]
    fn workflow_path_mirrors_audio_path_inside_private_directory() {
        let workspace = Path::new("/workspace");
        assert_eq!(
            workflow_local_path(workspace, "folder1/folder2/song.mp3"),
            workspace.join(".adbstudio/workflows/folder1/folder2/song.mp3.workflow.json")
        );
    }

    #[test]
    fn workflow_path_does_not_modify_workspace_index() {
        let workspace = tempfile_directory();
        let audio_path = workspace.join("downloads/song.mp3");
        assert_eq!(
            metadata::workflow_path(&workspace, &audio_path),
            Some(workspace.join(".adbstudio/workflows/downloads/song.mp3.workflow.json"))
        );
        assert!(!workspace.join(".adbstudio/index.json").exists());

        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn progress_counts_matching_basenames() {
        let directory = tempfile_directory();
        fs::write(directory.join("song.mp3"), b"audio").unwrap();
        let files = vec![
            RemoteFile {
                name: "nested/song.mp3".to_string(),
                path: "/remote/song.mp3".to_string(),
                modified: 2,
            },
            RemoteFile {
                name: "other.mp3".to_string(),
                path: "/remote/other.mp3".to_string(),
                modified: 1,
            },
        ];
        assert_eq!(
            progress_with_index(
                &files,
                &directory,
                &SyncConfig::default(),
                &DownloadIndex::default()
            )
            .unwrap()
            .present,
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn completed_record_counts_after_local_file_is_removed() {
        let workspace = tempfile_directory();
        let config = SyncConfig {
            url: "https://comfy.example".to_string(),
            ..SyncConfig::default()
        };
        let file = RemoteFile {
            name: "song.mp3".to_string(),
            path: "/output/audio/song.mp3".to_string(),
            modified: 0,
        };
        let mut index = DownloadIndex::default();
        index.record_completed(&config, &file, 1234);
        save_download_index(&workspace, &index).unwrap();

        let loaded = load_download_index(&workspace).unwrap();
        assert_eq!(loaded, index);
        assert_eq!(loaded.downloads[0].status, "completed");
        assert_eq!(loaded.downloads[0].size, 1234);
        assert_eq!(
            progress_with_index(&[file], &workspace, &config, &loaded)
                .unwrap()
                .present,
            1
        );
        fs::remove_dir_all(workspace).unwrap();
    }

    fn tempfile_directory() -> std::path::PathBuf {
        static TEST_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = TEST_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "adb-studio-sync-{}-{timestamp}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
