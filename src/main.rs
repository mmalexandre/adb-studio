use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use audio::playback::PlaybackEngine;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod audio;
mod metadata;
mod settings;
mod workspace;
use audio::{loader, waveform};
use metadata::AudioComment;
use workspace::file_system::{self, TreeState};

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");
use audio::loader::State as AudioLoadState;
use audio::view::{
    comment_rows, format_duration, format_seconds, scroll_to_path as scroll_audio_to_path,
    select_comment, selected_loop_range, update_audio_rows, update_comment_model,
};
use settings::AppSettings;
use workspace::workflow::{
    apply_workflow, clear_workflow, load_workflow_for_audio, scan_json_files,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    slint::set_xdg_app_id("com.adbstudio.AdbStudio")?;
    let settings = Rc::new(RefCell::new(settings::load()));
    settings::restore_window(&window, &mut settings.borrow_mut());
    let tree_state: Rc<RefCell<Option<TreeState>>> = Rc::new(RefCell::new(None));
    let audio_folder: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let workflow_files: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let audio_model: Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>> = Rc::new(RefCell::new(None));
    let comment_editor_original: Rc<RefCell<Option<AudioComment>>> = Rc::new(RefCell::new(None));
    let comment_editor_duration = Rc::new(RefCell::new(0.0_f32));
    let last_button_click: Rc<RefCell<Option<(PathBuf, Instant)>>> = Rc::new(RefCell::new(None));
    let (playback, playback_error) = match PlaybackEngine::new() {
        Ok(engine) => (Rc::new(RefCell::new(Some(engine))), None),
        Err(error) => (Rc::new(RefCell::new(None)), Some(error)),
    };
    if let Some(error) = playback_error {
        window.set_audio_error(error.into());
    }
    let (audio_result_sender, audio_result_receiver) = mpsc::channel();
    let audio_load_state = Arc::new(Mutex::new(AudioLoadState {
        folder: PathBuf::new(),
        paths: Vec::new(),
        requested_range: None,
        generated: std::collections::HashSet::new(),
        generation: 0,
        running: false,
        completed: 0,
        total: 0,
        result_sender: audio_result_sender,
    }));
    window.set_build_number(BUILD_NUMBER.into());
    window.set_light_theme(settings.borrow().light_theme);
    window.set_theme_index(if settings.borrow().light_theme { 1 } else { 0 });
    window.set_loop_enabled(settings.borrow().loop_enabled);
    window.set_audio_volume(1.0);
    window.set_left_pane_width(settings.borrow().left_pane_width.into());
    window.set_metadata_pane_height(settings.borrow().metadata_pane_height.into());
    window.set_metadata_visible(settings.borrow().metadata_visible);
    window.set_comment_background_hex(settings.borrow().comment_background_color.clone().into());
    window.set_comment_text_hex(settings.borrow().comment_text_color.clone().into());
    window.set_comment_background_color(settings::parse_color(
        &settings.borrow().comment_background_color,
        slint::Color::from_argb_u8(255, 0, 0, 0),
    ));
    window.set_comment_text_color(settings::parse_color(
        &settings.borrow().comment_text_color,
        slint::Color::from_argb_u8(255, 255, 255, 255),
    ));

    {
        let playback = Rc::clone(&playback);
        window.on_volume_changed(move |volume| {
            if let Some(engine) = playback.borrow_mut().as_mut() {
                engine.set_volume(volume);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
        window.on_comment_range_requested(move |path, start, end| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&folder, &path, &playback);
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            select_comment(&audio_model, &path, start, end);
            *comment_editor_original.borrow_mut() = None;
            *comment_editor_duration.borrow_mut() = duration;
            window.set_comment_editor_path(path.to_string_lossy().into_owned().into());
            window.set_comment_editor_start(format_seconds(start * duration).into());
            window.set_comment_editor_end(format_seconds(end * duration).into());
            window.set_comment_editor_text("".into());
            window.set_comment_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_audio_row_selected(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            select_tree_path(&window, &tree_state, &settings, Path::new(path.as_str()));
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        window.on_rename_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = if !window.get_tree_menu_path().is_empty() {
                PathBuf::from(window.get_tree_menu_path().as_str())
            } else {
                let state_ref = tree_state.borrow();
                let Some(state) = state_ref.as_ref() else {
                    return;
                };
                let Some(path) = state.selected.as_ref() else {
                    return;
                };
                path.clone()
            };
            if !path.exists() || path.file_name().is_none() {
                return;
            }
            window.set_tree_edit_path(path.to_string_lossy().into_owned().into());
            window.set_tree_edit_text(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_tree_edit_mode(1);
            window.set_tree_menu_path("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        window.on_new_folder_requested(move |parent| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let parent = PathBuf::from(parent.as_str());
            let parent = if parent.is_dir() {
                parent
            } else if parent.is_file() {
                let Some(parent) = parent.parent() else {
                    return;
                };
                parent.to_path_buf()
            } else {
                return;
            };
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_and_expand(&parent);
            }
            window.set_tree_edit_path(parent.to_string_lossy().into_owned().into());
            window.set_tree_edit_text("".into());
            window.set_tree_edit_mode(2);
            window.set_tree_menu_path("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_tree_edit_accepted(move |path, text, mode| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let name = text.trim();
            if name.is_empty()
                || name == "."
                || name == ".."
                || name
                    .chars()
                    .any(|character| character == '/' || character == '\\')
            {
                window.set_tree_edit_path("".into());
                window.set_tree_edit_text("".into());
                window.set_tree_edit_mode(0);
                return;
            }
            let source = PathBuf::from(path.as_str());
            let destination = if mode == 2 {
                source.join(name)
            } else {
                let Some(parent) = source.parent() else {
                    return;
                };
                parent.join(name)
            };
            let result = if mode == 2 {
                fs::create_dir(&destination)
            } else {
                fs::rename(&source, &destination)
            };
            if let Err(error) = result {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_and_expand(&destination);
            }
            settings.borrow_mut().last_selected_path =
                Some(destination.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            window.set_selected_name(
                destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_tree_edit_path("".into());
            window.set_tree_edit_text("".into());
            window.set_tree_edit_mode(0);
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_tree_drop_requested(move |source, source_index, pointer_y| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let source = PathBuf::from(source.as_str());
            let (sources, target) = {
                let state_ref = tree_state.borrow();
                let Some(state) = state_ref.as_ref() else {
                    return;
                };
                let rows = file_system::build_visible_rows(state);
                let target_index = (source_index as f32 + ((pointer_y - 13.0) / 26.0).round())
                    .clamp(0.0, (rows.len() - 1) as f32)
                    as usize;
                let Some(row) = rows.get(target_index) else {
                    return;
                };
                let mut sources = state.selected_paths();
                if !sources.iter().any(|path| path == &source) {
                    sources = vec![source.clone()];
                }
                (sources, PathBuf::from(&row.path))
            };
            if sources.is_empty() || !target.is_dir() {
                return;
            }
            let mut destinations = Vec::with_capacity(sources.len());
            for source in &sources {
                let Some(name) = source.file_name() else {
                    return;
                };
                let Some(parent) = source.parent() else {
                    return;
                };
                if !source.exists() || source == &target || parent == target {
                    return;
                }
                if source.is_dir() && target.strip_prefix(source).is_ok() {
                    window.set_audio_error(
                        "File operation: cannot move a folder into itself".into(),
                    );
                    return;
                }
                let destination = target.join(name);
                if destination.exists() || destinations.contains(&destination) {
                    window.set_audio_error("File operation: destination already exists".into());
                    return;
                }
                destinations.push(destination);
            }
            for (source, destination) in sources.iter().zip(&destinations) {
                if let Err(error) = fs::rename(source, destination) {
                    window.set_audio_error(format!("File operation: {error}").into());
                    return;
                }
            }
            {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.select_paths(destinations.clone(), destinations.last().cloned().unwrap());
            }
            let primary = destinations.last().unwrap();
            settings.borrow_mut().last_selected_path = Some(primary.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            window.set_selected_name(
                primary
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_tree_edit_cancelled(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_tree_edit_path("".into());
                window.set_tree_edit_text("".into());
                window.set_tree_edit_mode(0);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let workflow_audio_folder = Rc::clone(&audio_folder);
        let workflow_files_for_search = Rc::clone(&workflow_files);
        window.on_metadata_toggle(move || {
            if let Some(window) = weak_window.upgrade() {
                let visible = !window.get_metadata_visible();
                window.set_metadata_visible(visible);
                let mut settings = settings.borrow_mut();
                settings.metadata_visible = visible;
                settings::save(&settings);
            }
        });
        let weak_window = window.as_weak();
        window.on_workflow_selected(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = workflow_audio_folder.borrow().clone() else {
                return;
            };
            let audio_path = window.get_active_audio_path().to_string();
            if !audio_path.is_empty() {
                let mut index = metadata::load_index(&folder);
                if let Some(file) = index
                    .audio_files
                    .iter_mut()
                    .find(|file| file.file_path == audio_path)
                {
                    file.workflow_json_path = Some(path.to_string());
                    metadata::save_index(&folder, &index);
                }
            }
            match metadata::comfyui::parse_file(Path::new(path.as_str())) {
                Ok(workflow) => apply_workflow(&window, &folder, path.as_str(), workflow),
                Err(error) => window.set_audio_error(format!("Workflow JSON: {error}").into()),
            }
        });
        let weak_window = window.as_weak();
        window.on_workflow_search_changed(move |query| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let query = query.to_ascii_lowercase();
            let rows = workflow_files_for_search
                .borrow()
                .iter()
                .filter(|(name, _)| name.to_ascii_lowercase().contains(&query))
                .map(|(name, path)| WorkflowFileRow {
                    name: name.clone().into(),
                    path: path.clone().into(),
                })
                .collect::<Vec<_>>();
            window.set_workflow_json_files(ModelRc::new(VecModel::from(rows)));
        });
        let weak_window = window.as_weak();
        window.on_lora_edit_requested(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_lora_editor_filename(filename);
            window.set_lora_editor_tag(tag);
            window.set_lora_editor_visible(true);
        });
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        window.on_lora_save(move |filename, tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let mut index = metadata::load_index(&folder);
            if let Some(lora) = index
                .loras
                .iter_mut()
                .find(|lora| lora.filename == filename.as_str())
            {
                lora.custom_tag = tag.to_string();
            } else {
                index.loras.push(metadata::LoraMetadata {
                    filename: filename.to_string(),
                    custom_tag: tag.to_string(),
                });
            }
            metadata::save_index(&folder, &index);
            window.set_lora_editor_visible(false);
            let audio_path = window.get_active_audio_path().to_string();
            if !audio_path.is_empty() {
                load_workflow_for_audio(&window, &folder, Path::new(&audio_path));
            }
        });
        let weak_window = window.as_weak();
        window.on_lora_cancel(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_lora_editor_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
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
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        window.on_rating_requested(move |path, rating| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let mut index = metadata::load_index(&folder);
            if let Some(file) = index
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path_string)
            {
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
                            AudioRow {
                                path: row.path,
                                name: row.name,
                                modified_date: row.modified_date,
                                peaks: row.peaks,
                                comments: row.comments,
                                rating: rating.clamp(0, 5),
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
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        window.on_comment_range_moved(move |path, old_start, old_end, start, end, text| {
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path_string = path.to_string();
            let mut index = metadata::load_index(&folder);
            let comments = {
                let Some(file) = index
                    .audio_files
                    .iter_mut()
                    .find(|item| item.file_path == path_string)
                else {
                    return;
                };
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
                comment_rows(file)
            };
            metadata::save_index(&folder, &index);
            update_comment_model(&audio_model, Path::new(path.as_str()), comments);
            select_comment(&audio_model, Path::new(path.as_str()), start, end);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        window.on_comment_selected(move |path, start, _end, _text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let duration = comment_duration(&folder, &path, &playback);
            if duration <= 0.0 {
                window.set_audio_error("Unable to determine audio duration".into());
                return;
            }
            select_comment(&audio_model, &path, start, _end);
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            let position =
                Duration::from_secs_f32((start.clamp(0.0, 1.0) * duration).min(duration));
            let result = if engine.path() == Some(path.as_path()) && engine.can_resume() {
                let result = engine.seek(position);
                if result.is_ok() {
                    engine.resume();
                }
                result
            } else {
                engine.play(&path, position)
            };
            if let Err(error) = result {
                window.set_audio_error(error.into());
                return;
            }
            window.set_audio_error("".into());
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_playing(engine.is_playing());
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            save_playback_position(&folder, engine);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let playback = Rc::clone(&playback);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
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
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        let comment_editor_duration = Rc::clone(&comment_editor_duration);
        window.on_comment_save(move |path, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let (Ok(start_seconds), Ok(end_seconds)) = (start.parse::<f32>(), end.parse::<f32>())
            else {
                window.set_audio_error("Comment times must be numbers".into());
                return;
            };
            let duration = metadata::load_index(&folder)
                .audio_files
                .iter()
                .find(|item| item.file_path == path.to_string_lossy())
                .map(|item| item.duration_seconds)
                .filter(|duration| *duration > 0.0)
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
            let mut index = metadata::load_index(&folder);
            let Some(file) = index
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path.to_string_lossy())
            else {
                index.audio_files.push(metadata::AudioFileMetadata {
                    file_path: path.to_string_lossy().into_owned(),
                    comments: vec![comment],
                    ..Default::default()
                });
                metadata::save_index(&folder, &index);
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
                select_comment(&audio_model, &path, selected_start, selected_end);
                window.set_comment_editor_visible(false);
                return;
            };
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
            }
            file.comments.push(comment);
            metadata::save_index(&folder, &index);
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                folder,
            );
            select_comment(&audio_model, &path, selected_start, selected_end);
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let comment_editor_original = Rc::clone(&comment_editor_original);
        window.on_comment_delete(move |path, _start, _end, _text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let mut index = metadata::load_index(&folder);
            let Some(file) = index
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path.as_str())
            else {
                return;
            };
            if let Some(original) = comment_editor_original.borrow_mut().take() {
                file.comments.retain(|item| item != &original);
                metadata::save_index(&folder, &index);
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
            }
            window.set_comment_editor_visible(false);
        });
    }

    {
        let weak_window = window.as_weak();
        let comment_editor_original = Rc::clone(&comment_editor_original);
        window.on_comment_cancel(move || {
            *comment_editor_original.borrow_mut() = None;
            if let Some(window) = weak_window.upgrade() {
                window.set_comment_editor_visible(false);
            }
        });
    }

    {
        let settings = Rc::clone(&settings);
        window.on_left_pane_width_changed(move |width| {
            settings.borrow_mut().left_pane_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(&settings);
        window.on_metadata_pane_height_changed(move |height| {
            settings.borrow_mut().metadata_pane_height = height;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    let last_folder = settings.borrow().last_folder.clone();
    if let Some(last_folder) = last_folder {
        let folder = PathBuf::from(last_folder);
        if folder.is_dir() {
            set_workspace(
                &window,
                folder,
                &settings,
                &tree_state,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                &workflow_files,
            );
        }
    }

    {
        let weak_window = window.as_weak();
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let playback = Rc::clone(&playback);
        let audio_folder = Rc::clone(&audio_folder);
        let settings = Rc::clone(&settings);
        let last_persisted_position = Rc::new(RefCell::new(Instant::now()));
        let last_persisted_position_for_timer = Rc::clone(&last_persisted_position);
        let audio_result_receiver = Rc::new(RefCell::new(audio_result_receiver));
        let mut spinner_frame = 0usize;
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(40),
            move || {
                if let Some(window) = weak_window.upgrade() {
                    if let Some(engine) = playback.borrow_mut().as_mut() {
                        engine.update_position();
                        let selected_loop = engine
                            .path()
                            .and_then(|path| selected_loop_range(&audio_model, path));
                        let should_loop = selected_loop
                            .map(|(_, end)| engine.position() >= engine.duration().mul_f32(end))
                            .unwrap_or_else(|| {
                                !engine.is_playing() && engine.position() >= engine.duration()
                            });
                        if settings.borrow().loop_enabled
                            && !engine.duration().is_zero()
                            && should_loop
                        {
                            if let Some(path) = engine.path().map(Path::to_path_buf) {
                                let loop_start = selected_loop
                                    .map(|(start, _)| engine.duration().mul_f32(start))
                                    .unwrap_or(Duration::ZERO);
                                let _ = engine.play(&path, loop_start);
                            }
                        }
                        let active_path = engine
                            .path()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let position = engine.position();
                        let duration = engine.duration();
                        let playing = engine.is_playing();
                        window.set_active_audio_path(active_path.into());
                        window.set_audio_file_name(
                            engine
                                .path()
                                .and_then(|path| path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                                .into(),
                        );
                        window.set_audio_playing(playing);
                        window.set_audio_current_time(format_duration(position).into());
                        window.set_audio_total_duration(format_duration(duration).into());
                        update_audio_rows(&audio_model, engine.path(), playing, position, duration);
                        if last_persisted_position_for_timer.borrow().elapsed()
                            >= Duration::from_millis(500)
                        {
                            if let Some(folder) = audio_folder.borrow().clone() {
                                save_playback_position(&folder, engine);
                            }
                            *last_persisted_position_for_timer.borrow_mut() = Instant::now();
                        }
                    }
                    let state = audio_load_state.lock().unwrap();
                    window.set_audio_loading(state.running);
                    window.set_audio_completed(state.completed as i32);
                    window.set_audio_total(state.total as i32);
                    if state.running {
                        window.set_audio_spinner(["|", "/", "-", "\\"][spinner_frame].into());
                        spinner_frame = (spinner_frame + 1) % 4;
                    }
                }
                let Some(model) = audio_model.borrow().clone() else {
                    return;
                };
                for result in audio_result_receiver.borrow_mut().try_iter().take(3) {
                    let current_generation = audio_load_state.lock().unwrap().generation;
                    if result.generation != current_generation || result.index >= model.row_count()
                    {
                        continue;
                    }
                    let Some(row) = model.row_data(result.index) else {
                        continue;
                    };
                    model.set_row_data(
                        result.index,
                        AudioRow {
                            path: result.path.into(),
                            name: row.name,
                            modified_date: row.modified_date,
                            peaks: ModelRc::new(VecModel::from(waveform::aggregate_peaks(
                                &result.peaks,
                            ))),
                            comments: row.comments,
                            rating: row.rating,
                            is_active: row.is_active,
                            is_playing: row.is_playing,
                            progress: row.progress,
                            loop_enabled: row.loop_enabled,
                            selected_comment_start: row.selected_comment_start,
                            selected_comment_end: row.selected_comment_end,
                        },
                    );
                }
            },
        );
        std::mem::forget(timer);
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        let tree_state = Rc::clone(&tree_state);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_open_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };

            if let Some(folder) = rfd::FileDialog::new()
                .set_title("Open Adb Studio Workspace")
                .pick_folder()
            {
                set_workspace(
                    &window,
                    folder,
                    &settings,
                    &tree_state,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    &workflow_files,
                );
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_loop_changed(move |enabled| {
            settings.borrow_mut().loop_enabled = enabled;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_loop_enabled(enabled);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        let playback = Rc::clone(&playback);
        window.on_row_clicked(move |path, shift| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());

            let mut state_ref = tree_state.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return;
            };
            if path.is_dir() {
                state.toggle(&path);
            }
            state.select_with_shift(&path, shift);
            if !path.is_dir() {
                state.expand_to(&path);
            }
            let selected_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            drop(state_ref);

            window.set_selected_name(selected_name.into());
            refresh_tree(&window, &tree_state);
            if path.is_dir() {
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    path,
                );
            } else if file_system::FileKind::from_path(&path) == file_system::FileKind::Audio {
                let mut playback_ref = playback.borrow_mut();
                let Some(engine) = playback_ref.as_mut() else {
                    return;
                };
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
                window.set_audio_error("".into());
                window.set_active_audio_path(path.to_string_lossy().into_owned().into());
                window.set_audio_file_name(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .into(),
                );
                window.set_audio_playing(engine.is_playing());
                update_audio_rows(
                    &audio_model,
                    engine.path(),
                    engine.is_playing(),
                    engine.position(),
                    engine.duration(),
                );
                scroll_audio_to_path(&window, &audio_model, &path);
                if let Some(folder) = audio_folder.borrow().clone() {
                    load_workflow_for_audio(&window, &folder, &path);
                    save_playback_position(&folder, engine);
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(&audio_folder);
        let audio_model = Rc::clone(&audio_model);
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_filter_changed(move |filter| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_audio_filter(filter);
            let folder = audio_folder.borrow().clone();
            if let Some(folder) = folder {
                refresh_audio(
                    &window,
                    &audio_folder,
                    &audio_model,
                    &audio_load_state,
                    folder,
                );
            }
        });
    }

    {
        let audio_load_state = Arc::clone(&audio_load_state);
        window.on_audio_viewport_changed(move |start_index| {
            request_audio_generation(&audio_load_state, start_index as usize);
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        window.on_audio_play_pause(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            if engine.is_playing() {
                engine.pause();
            } else if engine.path().is_some() {
                engine.resume();
            } else {
                return;
            }
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            window.set_audio_playing(engine.is_playing());
            if let Some(folder) = audio_folder.borrow().clone() {
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let last_button_click = Rc::clone(&last_button_click);
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_audio_play(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let now = Instant::now();
            let restart =
                last_button_click
                    .borrow()
                    .as_ref()
                    .is_some_and(|(last_path, last_time)| {
                        last_path == &path && last_time.elapsed() <= Duration::from_millis(350)
                    });
            *last_button_click.borrow_mut() = Some((path.clone(), now));
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            if restart {
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
            } else if engine.path() == Some(path.as_path()) && engine.can_resume() {
                if engine.is_playing() {
                    engine.pause();
                } else {
                    engine.resume();
                }
            } else {
                if let Err(error) = engine.play(&path, Duration::ZERO) {
                    window.set_audio_error(error.into());
                    return;
                }
            }
            select_tree_path(&window, &tree_state, &settings, &path);
            window.set_audio_error("".into());
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
            window.set_audio_playing(engine.is_playing());
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            scroll_audio_to_path(&window, &audio_model, &path);
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, &path);
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let playback = Rc::clone(&playback);
        let audio_model = Rc::clone(&audio_model);
        let audio_folder = Rc::clone(&audio_folder);
        let tree_state = Rc::clone(&tree_state);
        let settings = Rc::clone(&settings);
        window.on_audio_seek(move |path, progress| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut playback_ref = playback.borrow_mut();
            let Some(engine) = playback_ref.as_mut() else {
                return;
            };
            let progress = progress.clamp(0.0, 1.0);
            let result = if engine.path() == Some(path.as_path()) {
                engine.seek(engine.duration().mul_f32(progress))
            } else {
                engine
                    .play(&path, Duration::ZERO)
                    .and_then(|_| engine.seek(engine.duration().mul_f32(progress)))
            };
            if let Err(error) = result {
                window.set_audio_error(error.into());
                return;
            }
            window.set_audio_error("".into());
            select_tree_path(&window, &tree_state, &settings, &path);
            window.set_active_audio_path(path.to_string_lossy().into_owned().into());
            window.set_audio_file_name(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
            update_audio_rows(
                &audio_model,
                engine.path(),
                engine.is_playing(),
                engine.position(),
                engine.duration(),
            );
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, &path);
                save_playback_position(&folder, engine);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_about(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_about_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_settings(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_settings_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_toggle_fullscreen(move || {
            if let Some(window) = weak_window.upgrade() {
                let fullscreen = window.window().is_fullscreen();
                window.window().set_fullscreen(!fullscreen);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_theme_selected(move |light_theme| {
            {
                settings.borrow_mut().light_theme = light_theme;
            }
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_theme_index(if light_theme { 1 } else { 0 });
                window.set_light_theme(light_theme);
            }
        });
    }

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
    settings::save_window(&window, &mut settings.borrow_mut());
    settings::save(&settings.borrow());
    Ok(())
}

fn set_workspace(
    window: &MainWindow,
    folder: PathBuf,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    workflow_files: &Rc<RefCell<Vec<(String, String)>>>,
) {
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"))
        .to_string();

    window.set_folder_name(folder_name.into());
    window.set_has_folder(true);
    let scanned_workflows = scan_json_files(&folder);
    *workflow_files.borrow_mut() = scanned_workflows.clone();
    window.set_workflow_json_files(ModelRc::new(VecModel::from(
        scanned_workflows
            .into_iter()
            .map(|(name, path)| WorkflowFileRow {
                name: name.into(),
                path: path.into(),
            })
            .collect::<Vec<_>>(),
    )));
    window.set_selected_workflow("".into());
    window.set_audio_file_name("".into());
    clear_workflow(window);

    {
        settings.borrow_mut().last_folder = Some(folder.to_string_lossy().into_owned());
    }
    let settings_snapshot = settings.borrow().clone();
    settings::save(&settings_snapshot);

    let mut new_tree_state = TreeState::new(folder.clone());
    if let Some(selected_path) = settings_snapshot
        .last_selected_path
        .as_deref()
        .map(PathBuf::from)
    {
        if selected_path != folder
            && selected_path.exists()
            && selected_path.strip_prefix(&folder).is_ok()
        {
            new_tree_state.select_and_expand(&selected_path);
            window.set_selected_name(
                selected_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .into(),
            );
        } else {
            window.set_selected_name("".into());
        }
    } else {
        window.set_selected_name("".into());
    }
    *tree_state.borrow_mut() = Some(new_tree_state);
    refresh_tree(window, tree_state);
    let audio_view_folder = settings_snapshot
        .last_selected_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|selected_path| {
            selected_path != &folder
                && selected_path.exists()
                && selected_path.strip_prefix(&folder).is_ok()
        })
        .and_then(|selected_path| {
            if selected_path.is_dir() {
                Some(selected_path)
            } else {
                selected_path.parent().map(Path::to_path_buf)
            }
        })
        .unwrap_or(folder);
    refresh_audio(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        audio_view_folder,
    );
}

fn refresh_audio(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
) {
    let filter = window.get_audio_filter().to_string();
    let mut rows = Vec::new();
    for entry in file_system::read_dir_sorted(&folder) {
        if entry.kind != file_system::FileKind::Audio || !matches_audio_filter(&entry.name, &filter)
        {
            continue;
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let path_string = entry.path.to_string_lossy().into_owned();
        let stored_position = metadata::load_index(&folder)
            .audio_files
            .into_iter()
            .find(|item| item.file_path == path_string);
        let comments = stored_position
            .as_ref()
            .map(comment_rows)
            .unwrap_or_else(|| ModelRc::new(VecModel::from(Vec::new())));
        let progress = stored_position
            .as_ref()
            .filter(|item| item.duration_seconds > 0.0)
            .map(|item| (item.last_position_seconds / item.duration_seconds).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        rows.push(AudioRow {
            path: path_string.into(),
            name: entry.name.into(),
            modified_date: modified_date.to_string().into(),
            peaks: ModelRc::new(VecModel::from(vec![0.0; waveform::DISPLAY_PEAK_COUNT])),
            comments,
            rating: stored_position
                .as_ref()
                .map(|item| item.normalized_rating() as i32)
                .unwrap_or(0),
            is_active: false,
            is_playing: false,
            progress,
            loop_enabled: false,
            selected_comment_start: -1.0,
            selected_comment_end: -1.0,
        });
    }
    rows.sort_by(|left, right| right.modified_date.cmp(&left.modified_date));
    let paths: Vec<PathBuf> = rows
        .iter()
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect();
    let total = paths.len();
    *audio_folder.borrow_mut() = Some(folder);
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    *audio_model.borrow_mut() = Some(model);
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = audio_folder.borrow().clone().unwrap_or_default();
        state.paths = paths;
        state.generated.clear();
        state.generation += 1;
        state.completed = 0;
        state.total = total;
        state.requested_range = None;
    }
    request_audio_generation(audio_load_state, 0);
}

fn matches_audio_filter(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::matches_audio_filter;

    #[test]
    fn audio_filter_matches_names_case_insensitively() {
        assert!(matches_audio_filter("My Voice.WAV", " voice "));
        assert!(matches_audio_filter("My Voice.WAV", ""));
        assert!(!matches_audio_filter("My Voice.WAV", "music"));
    }
}

fn comment_duration(
    folder: &Path,
    path: &Path,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
) -> f32 {
    let path_string = path.to_string_lossy();
    let stored_duration = metadata::load_index(folder)
        .audio_files
        .iter()
        .find(|item| item.file_path == path_string)
        .map(|item| item.duration_seconds)
        .unwrap_or_default();
    if stored_duration > 0.0 {
        return stored_duration;
    }
    let mut playback_ref = playback.borrow_mut();
    let Some(engine) = playback_ref.as_mut() else {
        return 0.0;
    };
    if engine.path() != Some(path) && engine.play(path, Duration::ZERO).is_err() {
        return 0.0;
    }
    let duration = engine.duration().as_secs_f32();
    if duration > 0.0 {
        let mut index = metadata::load_index(folder);
        if let Some(file) = index
            .audio_files
            .iter_mut()
            .find(|item| item.file_path == path_string)
        {
            file.duration_seconds = duration;
        } else {
            index.audio_files.push(metadata::AudioFileMetadata {
                file_path: path_string.into_owned(),
                duration_seconds: duration,
                ..Default::default()
            });
        }
        metadata::save_index(folder, &index);
    }
    duration
}

fn save_playback_position(folder: &Path, engine: &PlaybackEngine) {
    let Some(path) = engine.path() else {
        return;
    };
    let path_string = path.to_string_lossy().into_owned();
    let mut index = metadata::load_index(folder);
    if let Some(stored) = index
        .audio_files
        .iter_mut()
        .find(|item| item.file_path == path_string)
    {
        stored.last_position_seconds = engine.position().as_secs_f32();
        stored.duration_seconds = engine.duration().as_secs_f32();
    } else {
        index.audio_files.push(metadata::AudioFileMetadata {
            file_path: path_string,
            last_position_seconds: engine.position().as_secs_f32(),
            duration_seconds: engine.duration().as_secs_f32(),
            ..Default::default()
        });
    }
    metadata::save_index(folder, &index);
}

fn select_tree_path(
    window: &MainWindow,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    settings: &Rc<RefCell<AppSettings>>,
    path: &Path,
) {
    let mut state_ref = tree_state.borrow_mut();
    let Some(state) = state_ref.as_mut() else {
        return;
    };
    state.select_and_expand(path);
    let tree_index = file_system::build_visible_rows(state)
        .iter()
        .position(|row| row.path == path)
        .map(|index| index as i32);
    settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
    let settings_snapshot = settings.borrow().clone();
    settings::save(&settings_snapshot);
    window.set_selected_name(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .into(),
    );
    drop(state_ref);
    refresh_tree(window, tree_state);
    window.set_tree_scroll_to_index(-1);
    window.set_tree_scroll_to_index(tree_index.unwrap_or(-1));
}

fn request_audio_generation(audio_load_state: &Arc<Mutex<AudioLoadState>>, start_index: usize) {
    loader::request(audio_load_state, start_index);
}

fn refresh_tree(window: &MainWindow, tree_state: &Rc<RefCell<Option<TreeState>>>) {
    let state_ref = tree_state.borrow();
    let Some(state) = state_ref.as_ref() else {
        return;
    };
    window.set_tree_selection_count(state.selected_paths().len() as i32);

    let rows: Vec<TreeRow> = file_system::build_visible_rows(state)
        .into_iter()
        .map(|row| TreeRow {
            path: row.path.to_string_lossy().into_owned().into(),
            name: row.name.into(),
            depth: row.depth,
            is_dir: row.is_dir,
            is_expanded: row.is_expanded,
            is_selected: row.is_selected,
            kind: row.kind.as_str().into(),
        })
        .collect();

    window.set_tree_rows(ModelRc::new(VecModel::from(rows)));
}
