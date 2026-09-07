use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    audio::{
        loader::State as AudioLoadState,
        playback::PlaybackEngine,
        session::comment_duration,
        view::{comment_rows, format_seconds, select_comment, update_comment_model},
    },
    metadata::{self, AudioComment},
    settings::{self, AppSettings},
    sync::{ComfyUiClient, SyncConfig, WorkflowRunUpdate},
    workspace::{
        file_system::TreeState,
        library::refresh_audio,
        tree_nav::select_tree_path,
        workflow::load_workflow_for_audio,
    },
    MainWindow,
};

pub fn register_workflow_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<crate::AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    workflow_loading: &Rc<RefCell<bool>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    workflow_run_cancelled: &Rc<RefCell<Option<Arc<AtomicBool>>>>,
    workflow_run_sender: &mpsc::Sender<WorkflowRunUpdate>,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    recreate_workflow_pending: &Rc<RefCell<bool>>,
    comment_editor_original: &Rc<RefCell<Option<AudioComment>>>,
    comment_editor_duration: &Rc<RefCell<f32>>,
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

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        window.on_comment_colors_selected(move |background, text| {
            let background = background.to_string();
            let text = text.to_string();
            let Some(background_color) = settings::parse_hex_color(&background) else {
                return;
            };
            let Some(text_color) = settings::parse_hex_color(&text) else {
                return;
            };
            {
                let mut settings = settings.borrow_mut();
                settings.comment_background_color = background.clone();
                settings.comment_text_color = text.clone();
            }
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_comment_background_hex(background.into());
                window.set_comment_text_hex(text.into());
                window.set_comment_background_color(background_color);
                window.set_comment_text_color(text_color);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        window.on_rating_requested(move |path, rating| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let mut index = metadata::load_index(&folder);
            if let Some(file) = index.audio_files.iter_mut().find(|item| item.file_path == path_string) {
                file.rating = rating.clamp(0, 5) as u8;
            } else {
                index.audio_files.push(metadata::AudioFileMetadata {
                    file_path: path_string.clone(),
                    rating: rating.clamp(0, 5) as u8,
                    ..Default::default()
                });
            }
            metadata::save_index(&folder, &index);
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    if row.path.as_str() == path_string {
                        model.set_row_data(
                            index,
                            crate::AudioRow {
                                path: row.path,
                                name: row.name,
                                modified_date: row.modified_date,
                                peaks: row.peaks,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: row.differences,
                                rating: rating.clamp(0, 5),
                                is_pinned: row.is_pinned,
                                is_selected: row.is_selected,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                            },
                        );
                        break;
                    }
                }
            }
            window.set_audio_error("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        window.on_comment_range_moved(move |path, old_start, old_end, start, end, text| {
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let audio_path = Path::new(path.as_str());
            let mut file = metadata::load_audio_metadata(&folder, audio_path);
            if file.file_path.is_empty() || file.file_path != path_string {
                return;
            }
            let duration = file.duration_seconds;
            if duration <= 0.0 {
                return;
            }
            if let Some(comment) = file.comments.iter_mut().find(|comment| {
                (comment.start_seconds / duration - old_start).abs() < 0.001
                    && (comment.end_seconds / duration - old_end).abs() < 0.001
                    && comment.text == text.as_str()
            }) {
                comment.start_seconds = (start * duration).clamp(0.0, duration);
                comment.end_seconds = (end * duration).clamp(comment.start_seconds, duration);
            } else {
                return;
            }
            let comments = comment_rows(&file);
            metadata::save_audio_metadata(&folder, audio_path, &file);
            update_comment_model(&audio_model, audio_path, comments);
            select_comment(&audio_model, audio_path, start, end);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let playback = Rc::clone(playback);
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let weak_window_for_move = weak_window.clone();
        let audio_folder_for_move = Rc::clone(&audio_folder);
        let audio_model_for_move = Rc::clone(&audio_model);
        let audio_load_state_for_move = Arc::clone(&audio_load_state);
        let playback_for_move = Rc::clone(&playback);
        let tree_state_for_move = Rc::clone(&tree_state);
        let settings_for_move = Rc::clone(&settings);
        let settings_for_request = Rc::clone(&settings);
        let move_to_trash: Rc<dyn Fn(SharedString)> = Rc::new(move |path| {
            let Some(window) = weak_window_for_move.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder_for_move.borrow().clone() else {
                return;
            };
            let source = PathBuf::from(path.as_str());
            let Some(name) = source.file_name() else {
                return;
            };
            let trash_folder = folder.join(".adbstudio").join("trash");
            let destination = trash_folder.join(name);
            if destination.exists() {
                window.set_audio_error("File operation: trash destination already exists".into());
                return;
            }
            if let Err(error) = fs::create_dir_all(&trash_folder).and_then(|_| fs::rename(&source, &destination)) {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            if playback_for_move.borrow().as_ref().and_then(|engine| engine.path()) == Some(source.as_path()) {
                if let Some(engine) = playback_for_move.borrow_mut().as_mut() {
                    engine.stop();
                }
                window.set_active_audio_path("".into());
                window.set_audio_file_name("".into());
                window.set_audio_playing(false);
            }
            let parent = source.parent().unwrap_or(&folder).to_path_buf();
            select_tree_path(&window, &tree_state_for_move, &settings_for_move, &parent);
            refresh_audio(&window, &audio_folder_for_move, &audio_model_for_move, &audio_load_state_for_move, parent);
            window.set_audio_error("".into());
        });
        let move_to_trash_for_request = Rc::clone(&move_to_trash);
        let weak_window_for_request = weak_window.clone();
        window.on_trash_requested(move |path| {
            let Some(window) = weak_window_for_request.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            if !settings_for_request.borrow().trash_confirmation_disabled_workspaces.contains(&folder.to_string_lossy().to_string()) {
                window.set_trash_confirm_path(path);
                window.set_trash_confirm_dont_ask(false);
                window.set_trash_confirm_visible(true);
                return;
            }
            move_to_trash_for_request(path);
        });

        let move_to_trash_for_confirmation = Rc::clone(&move_to_trash);
        let settings = Rc::clone(&settings);
        let weak_window_for_confirm = weak_window.clone();
        window.on_trash_confirmed(move |dont_ask| {
            let Some(window) = weak_window_for_confirm.upgrade() else {
                return;
            };
            let path = window.get_trash_confirm_path();
            if dont_ask {
                if let Some(folder) = settings.borrow().last_folder.clone() {
                    settings.borrow_mut().trash_confirmation_disabled_workspaces.insert(folder);
                    settings::save(&settings.borrow());
                }
            }
            window.set_trash_confirm_visible(false);
            if !path.is_empty() {
                move_to_trash_for_confirmation(path);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_trash_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_trash_confirm_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let playback = Rc::clone(playback);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let comment_editor_duration = Rc::clone(comment_editor_duration);
        window.on_comment_edit_requested(move |path, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&folder, &path, &playback);
            if duration <= 0.0 {
                return;
            }
            let original = AudioComment {
                start_seconds: start * duration,
                end_seconds: end * duration,
                text: text.to_string(),
            };
            *comment_editor_duration.borrow_mut() = duration;
            select_comment(&audio_model, &path, start, end);
            *comment_editor_original.borrow_mut() = Some(original.clone());
            window.set_comment_editor_path(path.to_string_lossy().into_owned().into());
            window.set_comment_editor_start(format_seconds(original.start_seconds).into());
            window.set_comment_editor_end(format_seconds(original.end_seconds).into());
            window.set_comment_editor_text(original.text.into());
            window.set_comment_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let comment_editor_duration = Rc::clone(comment_editor_duration);
        window.on_comment_save(move |path, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let (Ok(start_seconds), Ok(end_seconds)) = (start.parse::<f32>(), end.parse::<f32>()) else {
                window.set_audio_error("Comment times must be numbers".into());
                return;
            };
            let stored_metadata = metadata::load_audio_metadata(&folder, &path);
            let duration = (stored_metadata.duration_seconds > 0.0)
                .then_some(stored_metadata.duration_seconds)
                .unwrap_or(*comment_editor_duration.borrow());
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            let comment = AudioComment {
                start_seconds,
                end_seconds,
                text: text.to_string(),
            }
            .normalized(duration);
            let selected_start = comment.start_seconds / duration;
            let selected_end = comment.end_seconds / duration;
            let mut file = stored_metadata;
            file.file_path = path.to_string_lossy().into_owned();
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
            }
            file.comments.push(comment);
            metadata::save_audio_metadata(&folder, &path, &file);
            let view_folder = path.parent().unwrap_or(&folder).to_path_buf();
            refresh_audio(&window, &audio_folder, &audio_model, &audio_load_state, view_folder);
            select_comment(&audio_model, &path, selected_start, selected_end);
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        let comment_editor_original = Rc::clone(comment_editor_original);
        window.on_comment_delete(move |path, _start, _end, _text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut file = metadata::load_audio_metadata(&workspace, &path);
            if file.file_path.is_empty() {
                return;
            }
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
                metadata::save_audio_metadata(&workspace, &path, &file);
                let view_folder = path.parent().unwrap_or(&workspace).to_path_buf();
                refresh_audio(&window, &audio_folder, &audio_model, &audio_load_state, view_folder);
            }
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let comment_editor_original = Rc::clone(comment_editor_original);
        window.on_comment_cancel(move || {
            *comment_editor_original.borrow_mut() = None;
            if let Some(window) = weak_window.upgrade() {
                window.set_comment_editor_visible(false);
            }
        });
    }

    {
        let settings = Rc::clone(settings);
        window.on_left_pane_width_changed(move |width| {
            settings.borrow_mut().left_pane_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(settings);
        window.on_metadata_pane_height_changed(move |height| {
            settings.borrow_mut().metadata_pane_height = height;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
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
