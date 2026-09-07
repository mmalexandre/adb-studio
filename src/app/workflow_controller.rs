use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use slint::ComponentHandle;

use crate::{
    metadata::{self},
    settings::{self, AppSettings},
    sync::{ComfyUiClient, SyncConfig, WorkflowRunUpdate},
    workspace::workflow::load_workflow_for_audio,
    MainWindow,
};

pub fn register_workflow_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    workflow_loading: &Rc<RefCell<bool>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    workflow_run_cancelled: &Rc<RefCell<Option<Arc<AtomicBool>>>>,
    workflow_run_sender: &mpsc::Sender<WorkflowRunUpdate>,
    recreate_workflow_pending: &Rc<RefCell<bool>>,
) {
    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let recreate_workflow_pending = Rc::clone(recreate_workflow_pending);
        let edited_workflow = Rc::clone(edited_workflow);
        window.on_recreate_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before recreating a workflow".into());
                return;
            };
            let Some(config) = crate::sync::load_config(&folder).filter(|config| !config.url.is_empty())
            else {
                *recreate_workflow_pending.borrow_mut() = true;
                let config = crate::sync::load_config(&folder).unwrap_or_default();
                window.set_comfyui_sync_url(config.url.into());
                window.set_comfyui_sync_remote_directory(config.remote_directory.into());
                window.set_comfyui_sync_local_directory(config.local_directory.into());
                window.set_comfyui_sync_interval(config.interval_ms.to_string().into());
                window.set_comfyui_sync_error(
                    "Configure ComfyUI sync to recreate this workflow".into(),
                );
                window.set_comfyui_sync_test_message("".into());
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_visible(true);
                return;
            };
            recreate_workflow(&window, &folder, &config, &edited_workflow);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let edited_workflow = Rc::clone(edited_workflow);
        window.on_open_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before opening a workflow".into());
                return;
            };
            let selected_audio_path = window.get_selected_audio_path();
            let audio_path = Path::new(selected_audio_path.as_str());
            let Some(workflow_path) = metadata::workflow_path(&folder, audio_path) else {
                window.set_audio_error("Audio file is outside the workspace".into());
                return;
            };
            let path_to_open = if window.get_workflow_modified() {
                if edited_workflow.borrow().is_none() && !ensure_edit_copy(&window, &folder, &edited_workflow) {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                }
                let mut edited_workflow = edited_workflow.borrow_mut();
                let Some(workflow) = edited_workflow.as_mut() else {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                };
                for (field, value) in [
                    ("bpm", window.get_workflow_bpm().to_string()),
                    ("key", window.get_workflow_key().to_string()),
                    ("seed", window.get_workflow_seed().to_string()),
                    ("prompt", window.get_workflow_prompt().to_string()),
                    ("lyrics", window.get_workflow_lyrics().to_string()),
                ] {
                    metadata::comfyui::update_metadata(workflow, field, &value);
                }
                match write_modified_workflow(&workflow_path, workflow) {
                    Ok(path) => path,
                    Err(error) => {
                        window.set_audio_error(format!("Write modified workflow: {error}").into());
                        return;
                    }
                }
            } else {
                if !workflow_path.is_file() {
                    window.set_audio_error("Select a workflow before opening it".into());
                    return;
                }
                workflow_path
            };
            if let Err(error) = opener::open(&path_to_open) {
                window.set_audio_error(format!("Open workflow: {error}").into());
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let edited_workflow = Rc::clone(edited_workflow);
        let cancelled_state = Rc::clone(workflow_run_cancelled);
        let updates = workflow_run_sender.clone();
        window.on_run_workflow_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before running a workflow".into());
                return;
            };
            let audio_path = PathBuf::from(window.get_selected_audio_path().as_str());
            let Some(output_directory) = audio_path.parent().map(Path::to_path_buf) else {
                window.set_audio_error("Select an audio file before running a workflow".into());
                return;
            };
            let Some(output_stem) = audio_path.file_stem().and_then(|name| name.to_str()).map(str::to_owned) else {
                window.set_audio_error("Selected audio file has no usable name".into());
                return;
            };
            let workflow = if let Some(workflow) = edited_workflow.borrow().clone() {
                workflow
            } else {
                let Some(workflow_path) = metadata::workflow_path(&workspace, &audio_path) else {
                    window.set_audio_error("Audio file is outside the workspace".into());
                    return;
                };
                let Ok(contents) = fs::read_to_string(workflow_path) else {
                    window.set_audio_error("Select a workflow before running it in ComfyUI".into());
                    return;
                };
                let Ok(workflow) = serde_json::from_str(&contents) else {
                    window.set_audio_error("Workflow JSON is invalid".into());
                    return;
                };
                workflow
            };
            let Some(config) = crate::sync::load_config(&workspace).filter(|config| !config.url.is_empty()) else {
                window.set_audio_error("Configure ComfyUI sync before running a workflow".into());
                return;
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            *cancelled_state.borrow_mut() = Some(Arc::clone(&cancelled));
            window.set_comfyui_run_progress(0.0);
            window.set_comfyui_run_step("Submitting workflow to ComfyUI".into());
            window.set_comfyui_run_cancel_requested(false);
            window.set_comfyui_run_complete(false);
            window.set_comfyui_run_visible(true);
            let worker_updates = updates.clone();
            thread::spawn(move || {
                let result = ComfyUiClient::new().and_then(|client| {
                    client.run_workflow(
                        &config,
                        &workflow,
                        &workspace,
                        &output_directory,
                        &output_stem,
                        &cancelled,
                        &worker_updates,
                    )
                });
                if let Err(error) = result {
                    let _ = worker_updates.send(WorkflowRunUpdate::Error(error.to_string()));
                }
            });
        });
    }

    {
        let weak_window = window.as_weak();
        let cancelled_state = Rc::clone(workflow_run_cancelled);
        window.on_run_workflow_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_run_cancel_requested(true);
                window.set_comfyui_run_step("Cancelling on ComfyUI...".into());
            }
            if let Some(cancelled) = cancelled_state.borrow().as_ref() {
                cancelled.store(true, Ordering::Release);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let cancelled_state = Rc::clone(workflow_run_cancelled);
        window.on_run_workflow_closed(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_run_visible(false);
            }
            *cancelled_state.borrow_mut() = None;
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        window.on_metadata_toggle(move || {
            if let Some(window) = weak_window.upgrade() {
                let visible = !window.get_metadata_visible();
                window.set_metadata_visible(visible);
                let mut settings = settings.borrow_mut();
                settings.metadata_visible = visible;
                settings::save(&settings);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let edited_workflow_for_edit = Rc::clone(edited_workflow);
        let workflow_loading_for_edit = Rc::clone(workflow_loading);
        let audio_folder_for_edit = Rc::clone(audio_folder);
        window.on_workflow_metadata_changed(move |field, value| {
            if *workflow_loading_for_edit.borrow() {
                return;
            }
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            if window.get_workflow_loading() {
                return;
            }
            let Some(folder) = audio_folder_for_edit.borrow().clone() else {
                return;
            };
            if !ensure_edit_copy(&window, &folder, &edited_workflow_for_edit) {
                return;
            }
            window.set_workflow_modified(true);
            let _ = edited_workflow_for_edit.borrow_mut().as_mut().is_some_and(|workflow| {
                metadata::comfyui::update_metadata(workflow, field.as_str(), value.as_str())
            });
        });
    }

    {
        let weak_window = window.as_weak();
        let edited_workflow_for_number = Rc::clone(edited_workflow);
        let workflow_loading_for_number = Rc::clone(workflow_loading);
        let audio_folder_for_number = Rc::clone(audio_folder);
        window.on_workflow_number_changed(move |field, value| {
            if *workflow_loading_for_number.borrow() {
                return;
            }
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            if window.get_workflow_loading() {
                return;
            }
            let Some(folder) = audio_folder_for_number.borrow().clone() else {
                return;
            };
            if !ensure_edit_copy(&window, &folder, &edited_workflow_for_number) {
                return;
            }
            window.set_workflow_modified(true);
            let _ = edited_workflow_for_number.borrow_mut().as_mut().is_some_and(|workflow| {
                metadata::comfyui::update_metadata(workflow, field.as_str(), &value.to_string())
            });
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_lora_edit_requested(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_lora_editor_filename(filename);
            window.set_lora_editor_tag(tag);
            window.set_lora_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let workflow_loading = Rc::clone(workflow_loading);
        window.on_lora_save(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let mut index = metadata::load_index(&folder);
            if let Some(lora) = index.loras.iter_mut().find(|lora| lora.filename == filename.as_str()) {
                lora.custom_tag = tag.to_string();
            } else {
                index.loras.push(metadata::LoraMetadata {
                    filename: filename.to_string(),
                    custom_tag: tag.to_string(),
                });
            }
            metadata::save_index(&folder, &index);
            window.set_lora_editor_visible(false);
            let audio_path = window.get_selected_audio_path().to_string();
            if !audio_path.is_empty() {
                load_workflow_for_audio(&window, &folder, Path::new(&audio_path), &workflow_loading);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_lora_cancel(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_lora_editor_visible(false);
            }
        });
    }

}

fn recreate_workflow(
    window: &MainWindow,
    folder: &Path,
    config: &SyncConfig,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
) {
    let audio_path = window.get_selected_audio_path().to_string();
    if audio_path.is_empty() {
        window.set_audio_error("Select an audio file with a workflow first".into());
        return;
    }
    let Some(workflow_path) = metadata::workflow_path(folder, Path::new(&audio_path)) else {
        window.set_audio_error("Audio file is outside the workspace".into());
        return;
    };
    if !workflow_path.is_file() {
        window.set_audio_error("Select a workflow before recreating it".into());
        return;
    }
    let mut workflow = if let Some(workflow) = edited_workflow.borrow().clone() {
        workflow
    } else {
        match fs::read_to_string(&workflow_path)
            .map_err(|error| error.to_string())
            .and_then(|contents| {
                serde_json::from_str::<serde_json::Value>(&contents).map_err(|error| error.to_string())
            })
        {
            Ok(workflow) => workflow,
            Err(error) => {
                window.set_audio_error(format!("Workflow JSON: {error}").into());
                return;
            }
        }
    };
    for (field, value) in [
        ("bpm", window.get_workflow_bpm().to_string()),
        ("key", window.get_workflow_key().to_string()),
        ("seed", window.get_workflow_seed().to_string()),
        ("prompt", window.get_workflow_prompt().to_string()),
        ("lyrics", window.get_workflow_lyrics().to_string()),
    ] {
        metadata::comfyui::update_metadata(&mut workflow, field, &value);
    }
    match ComfyUiClient::new().and_then(|client| client.upload_workflow(config, &workflow)) {
        Ok(()) => {
            *edited_workflow.borrow_mut() = None;
            window.set_workflow_modified(false);
            match open_comfyui_workflow(config) {
                Ok(()) => window.set_audio_error("Workflow opened in ComfyUI".into()),
                Err(error) => window.set_audio_error(format!("ComfyUI opened upload failed: {error}").into()),
            }
        }
        Err(error) => window.set_audio_error(format!("ComfyUI: {error}").into()),
    }
}

fn ensure_edit_copy(
    window: &MainWindow,
    folder: &Path,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
) -> bool {
    if edited_workflow.borrow().is_some() {
        return true;
    }
    let audio_path = window.get_selected_audio_path().to_string();
    let Some(workflow_path) = metadata::workflow_path(folder, Path::new(&audio_path)) else {
        return false;
    };
    let Ok(contents) = fs::read_to_string(workflow_path) else {
        return false;
    };
    let Ok(workflow) = serde_json::from_str(&contents) else {
        return false;
    };
    *edited_workflow.borrow_mut() = Some(workflow);
    true
}

fn write_modified_workflow(workflow_path: &Path, workflow: &serde_json::Value) -> Result<PathBuf, String> {
    let filename = workflow_path.file_stem().and_then(|name| name.to_str()).unwrap_or("workflow");
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
    let modified_path = env::temp_dir().join(format!("{filename}-{}-{timestamp}.workflow.json", std::process::id()));
    let contents = serde_json::to_string_pretty(workflow).map_err(|error| error.to_string())?;
    fs::write(&modified_path, contents).map_err(|error| error.to_string())?;
    Ok(modified_path)
}

fn open_comfyui_workflow(config: &SyncConfig) -> Result<(), String> {
    let url = format!("{}/?adb-music-player=open-workflow", config.url);
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    command.arg(url).spawn().map(|_| ()).map_err(|error| error.to_string())
}
