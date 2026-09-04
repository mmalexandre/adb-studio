use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Sender},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use audio::playback::PlaybackEngine;
use display_info::DisplayInfo;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod audio;
mod file_system;
mod metadata;
mod waveform;
use file_system::TreeState;
use metadata::AudioComment;

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");
const AUDIO_PREFETCH_ROWS: usize = 32;
const AUDIO_PREFETCH_BEFORE: usize = 4;
struct AudioLoadState {
    folder: PathBuf,
    paths: Vec<PathBuf>,
    requested_range: Option<(usize, usize)>,
    generated: HashSet<PathBuf>,
    generation: u64,
    running: bool,
    completed: usize,
    total: usize,
    result_sender: Sender<AudioResult>,
}

struct AudioResult {
    generation: u64,
    index: usize,
    path: String,
    peaks: Vec<f32>,
}

#[derive(Clone, Deserialize, Serialize)]
struct AppSettings {
    last_folder: Option<String>,
    last_selected_path: Option<String>,
    light_theme: bool,
    #[serde(default)]
    loop_enabled: bool,
    #[serde(default = "default_left_pane_width")]
    left_pane_width: f32,
    #[serde(default = "default_metadata_pane_height")]
    metadata_pane_height: f32,
    #[serde(default)]
    metadata_visible: bool,
    #[serde(default = "default_comment_background_color")]
    comment_background_color: String,
    #[serde(default = "default_comment_text_color")]
    comment_text_color: String,
    #[serde(default)]
    window_width: Option<u32>,
    #[serde(default)]
    window_height: Option<u32>,
    #[serde(default)]
    window_x: Option<i32>,
    #[serde(default)]
    window_y: Option<i32>,
    #[serde(default)]
    window_maximized: bool,
}

fn default_left_pane_width() -> f32 {
    280.0
}

fn default_metadata_pane_height() -> f32 {
    190.0
}

fn default_comment_background_color() -> String {
    "#000000".to_owned()
}

fn default_comment_text_color() -> String {
    "#ffffff".to_owned()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_folder: None,
            last_selected_path: None,
            light_theme: false,
            loop_enabled: false,
            left_pane_width: default_left_pane_width(),
            metadata_pane_height: default_metadata_pane_height(),
            metadata_visible: false,
            comment_background_color: default_comment_background_color(),
            comment_text_color: default_comment_text_color(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: false,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    slint::set_xdg_app_id("com.adbstudio.AdbStudio")?;
    let settings = Rc::new(RefCell::new(load_settings()));
    restore_window_state(&window, &mut settings.borrow_mut());
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
        generated: HashSet::new(),
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
    window.set_comment_background_color(parse_color(&settings.borrow().comment_background_color, slint::Color::from_argb_u8(255, 0, 0, 0)));
    window.set_comment_text_color(parse_color(&settings.borrow().comment_text_color, slint::Color::from_argb_u8(255, 255, 255, 255)));

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
                || name.chars().any(|character| character == '/' || character == '\\')
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
            settings.borrow_mut().last_selected_path = Some(destination.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
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
            let target = {
                let state_ref = tree_state.borrow();
                let Some(state) = state_ref.as_ref() else {
                    return;
                };
                let rows = file_system::build_visible_rows(state);
                let target_index = (source_index as f32 + ((pointer_y - 13.0) / 26.0).round())
                    .clamp(0.0, (rows.len() - 1) as f32) as usize;
                let Some(row) = rows.get(target_index) else {
                    return;
                };
                PathBuf::from(&row.path)
            };
            let Some(name) = source.file_name() else {
                return;
            };
            let Some(parent) = source.parent() else {
                return;
            };
            if !source.exists() || !target.is_dir() || source == target || parent == target {
                return;
            }
            if source.is_dir() && target.strip_prefix(&source).is_ok() {
                window.set_audio_error("File operation: cannot move a folder into itself".into());
                return;
            }
            let destination = target.join(name);
            if destination.exists() {
                window.set_audio_error("File operation: destination already exists".into());
                return;
            }
            if let Err(error) = fs::rename(&source, &destination) {
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
            settings.borrow_mut().last_selected_path = Some(destination.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
            window.set_selected_name(
                destination
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
                save_settings(&settings);
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
            let Some(background_color) = parse_hex_color(&background) else {
                return;
            };
            let Some(text_color) = parse_hex_color(&text) else {
                return;
            };
            {
                let mut settings = settings.borrow_mut();
                settings.comment_background_color = background.clone();
                settings.comment_text_color = text.clone();
            }
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
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
            save_settings(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(&settings);
        window.on_metadata_pane_height_changed(move |height| {
            settings.borrow_mut().metadata_pane_height = height;
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
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
            save_settings(&settings_snapshot);
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
        window.on_row_clicked(move |path| {
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
            if path.is_dir() {
                state.select(&path);
            } else {
                state.select_and_expand(&path);
            }
            let selected_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
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
        let settings = Rc::clone(&settings);
        window.on_theme_selected(move |light_theme| {
            {
                settings.borrow_mut().light_theme = light_theme;
            }
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_theme_index(if light_theme { 1 } else { 0 });
                window.set_light_theme(light_theme);
            }
        });
    }

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
    save_window_state(&window, &mut settings.borrow_mut());
    save_settings(&settings.borrow());
    Ok(())
}

fn restore_window_state(window: &MainWindow, settings: &mut AppSettings) {
    let (Some(width), Some(height), Some(x), Some(y)) = (
        settings.window_width,
        settings.window_height,
        settings.window_x,
        settings.window_y,
    ) else {
        return;
    };

    let position_is_visible = DisplayInfo::all().map_or(false, |displays| {
        displays.iter().any(|display| {
            x >= display.x
                && y >= display.y
                && i64::from(x) < i64::from(display.x) + i64::from(display.width)
                && i64::from(y) < i64::from(display.y) + i64::from(display.height)
        })
    });

    if !position_is_visible {
        settings.window_width = None;
        settings.window_height = None;
        settings.window_x = None;
        settings.window_y = None;
        settings.window_maximized = false;
        return;
    }

    window.window().set_size(slint::PhysicalSize::new(width, height));
    window
        .window()
        .set_position(slint::PhysicalPosition::new(x, y));
    window.window().set_maximized(settings.window_maximized);
}

fn save_window_state(window: &MainWindow, settings: &mut AppSettings) {
    let size = window.window().size();
    let position = window.window().position();
    settings.window_width = Some(size.width);
    settings.window_height = Some(size.height);
    settings.window_x = Some(position.x);
    settings.window_y = Some(position.y);
    settings.window_maximized = window.window().is_maximized();
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
    save_settings(&settings_snapshot);

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

fn scan_json_files(folder: &Path) -> Vec<(String, String)> {
    fn visit(folder: &Path, files: &mut Vec<(String, String)>) {
        for entry in file_system::read_dir_sorted(folder) {
            if entry.is_dir {
                if entry.name != ".adbstudio" {
                    visit(&entry.path, files);
                }
            } else if entry.kind == file_system::FileKind::Json {
                files.push((
                    entry.name,
                    entry.path.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(folder, &mut files);
    files.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    files
}

fn clear_workflow(window: &MainWindow) {
    window.set_workflow_bpm("".into());
    window.set_workflow_key("".into());
    window.set_workflow_seed("".into());
    window.set_workflow_prompt("".into());
    window.set_workflow_lyrics("".into());
    window.set_workflow_loras(ModelRc::new(VecModel::from(Vec::<WorkflowLoraRow>::new())));
}

fn apply_workflow(
    window: &MainWindow,
    folder: &Path,
    path: &str,
    workflow: metadata::comfyui::ComfyUIWorkflow,
) {
    let display_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    window.set_selected_workflow(display_name.into());
    window.set_workflow_bpm(workflow.bpm.into());
    window.set_workflow_key(workflow.key.into());
    window.set_workflow_seed(workflow.seed.into());
    window.set_workflow_prompt(workflow.prompt.into());
    window.set_workflow_lyrics(workflow.lyrics.into());
    let index = metadata::load_index(folder);
    window.set_workflow_loras(ModelRc::new(VecModel::from(
        workflow
            .loras
            .into_iter()
            .map(|lora| WorkflowLoraRow {
                custom_tag: index
                    .loras
                    .iter()
                    .find(|stored| stored.filename == lora.filename)
                    .map(|stored| stored.custom_tag.clone())
                    .unwrap_or_default()
                    .into(),
                filename: lora.filename.into(),
                strength: lora.strength.into(),
            })
            .collect::<Vec<_>>(),
    )));
}

fn load_workflow_for_audio(window: &MainWindow, folder: &Path, path: &Path) {
    let path_string = path.to_string_lossy();
    let workflow_path = metadata::load_index(folder)
        .audio_files
        .iter()
        .find(|file| file.file_path == path_string)
        .and_then(|file| file.workflow_json_path.clone());
    let Some(workflow_path) = workflow_path else {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        return;
    };
    match metadata::comfyui::parse_file(Path::new(&workflow_path)) {
        Ok(workflow) => apply_workflow(window, folder, &workflow_path, workflow),
        Err(error) => window.set_audio_error(format!("Workflow JSON: {error}").into()),
    }
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
        if entry.kind != file_system::FileKind::Audio
            || !matches_audio_filter(&entry.name, &filter)
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

fn comment_rows(item: &metadata::AudioFileMetadata) -> ModelRc<CommentRow> {
    let duration = item.duration_seconds.max(0.001);
    let mut normalized: Vec<(f32, f32, String)> = item
        .comments
        .iter()
        .map(|comment| {
            (
                (comment.start_seconds / duration).clamp(0.0, 1.0),
                (comment.end_seconds / duration).clamp(0.0, 1.0),
                comment.text.clone(),
            )
        })
        .collect();
    normalized.sort_by(|left, right| left.0.total_cmp(&right.0));
    let rows = normalized
        .iter()
        .enumerate()
        .map(|(index, (start, end, text))| CommentRow {
            start: *start,
            end: *end,
            bubble_end: normalized
                .get(index + 1)
                .map(|next| next.0)
                .unwrap_or(1.0)
                .max(*start),
            text: text.clone().into(),
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(rows))
}

fn update_comment_model(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    path: &Path,
    comments: ModelRc<CommentRow>,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        if Path::new(row.path.as_str()) == path {
            if let Some(comment_model) =
                row.comments.as_any().downcast_ref::<VecModel<CommentRow>>()
            {
                comment_model.set_vec(comments.iter().collect::<Vec<_>>());
            }
            break;
        }
    }
}

fn update_audio_rows(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    active_path: Option<&Path>,
    is_playing: bool,
    position: Duration,
    duration: Duration,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    let progress = if duration.is_zero() {
        0.0
    } else {
        (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_active = active_path.is_some_and(|path| path == Path::new(row.path.as_str()));
        if row.is_active != is_active
            || row.is_playing != (is_active && is_playing)
            || (is_active && (row.progress - progress).abs() > 0.001)
        {
            model.set_row_data(
                index,
                AudioRow {
                    path: row.path,
                    name: row.name,
                    modified_date: row.modified_date,
                    peaks: row.peaks,
                    comments: row.comments,
                    rating: row.rating,
                    is_active,
                    is_playing: is_active && is_playing,
                    progress: if is_active { progress } else { row.progress },
                    loop_enabled: row.loop_enabled,
                    selected_comment_start: row.selected_comment_start,
                    selected_comment_end: row.selected_comment_end,
                },
            );
        }
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

fn selected_loop_range(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    path: &Path,
) -> Option<(f32, f32)> {
    let model = audio_model.borrow().clone()?;
    (0..model.row_count()).find_map(|index| {
        let row = model.row_data(index)?;
        if Path::new(row.path.as_str()) == path
            && row.selected_comment_start >= 0.0
            && row.selected_comment_end > row.selected_comment_start
        {
            Some((
                row.selected_comment_start.clamp(0.0, 1.0),
                row.selected_comment_end.clamp(0.0, 1.0),
            ))
        } else {
            None
        }
    })
}

fn select_comment(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    path: &Path,
    start: f32,
    end: f32,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let selected = Path::new(row.path.as_str()) == path;
        if selected || row.selected_comment_start >= 0.0 || row.selected_comment_end >= 0.0 {
            model.set_row_data(
                index,
                AudioRow {
                    path: row.path,
                    name: row.name,
                    modified_date: row.modified_date,
                    peaks: row.peaks,
                    comments: row.comments,
                    rating: row.rating,
                    is_active: row.is_active,
                    is_playing: row.is_playing,
                    progress: row.progress,
                    loop_enabled: row.loop_enabled,
                    selected_comment_start: if selected { start } else { -1.0 },
                    selected_comment_end: if selected { end } else { -1.0 },
                },
            );
        }
    }
}

fn format_seconds(seconds: f32) -> String {
    format!("{seconds:.3}")
}

fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
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
    settings.borrow_mut().last_selected_path = Some(path.to_string_lossy().into_owned());
    let settings_snapshot = settings.borrow().clone();
    save_settings(&settings_snapshot);
    window.set_selected_name(
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .into(),
    );
    drop(state_ref);
    refresh_tree(window, tree_state);
}

fn scroll_audio_to_path(
    window: &MainWindow,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    path: &Path,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    let Some(index) = (0..model.row_count()).find(|index| {
        model
            .row_data(*index)
            .is_some_and(|row| Path::new(row.path.as_str()) == path)
    }) else {
        return;
    };
    window.set_audio_scroll_to_index(index as i32);
}

fn request_audio_generation(audio_load_state: &Arc<Mutex<AudioLoadState>>, start_index: usize) {
    let mut state = audio_load_state.lock().unwrap();
    let start = start_index.saturating_sub(AUDIO_PREFETCH_BEFORE);
    let end = start_index
        .saturating_add(AUDIO_PREFETCH_ROWS)
        .min(state.paths.len());
    state.requested_range = Some((start, end));
    if state.running {
        return;
    }
    state.running = true;
    let shared_state = Arc::clone(audio_load_state);
    thread::spawn(move || generate_audio_ranges(shared_state));
}

fn generate_audio_ranges(audio_load_state: Arc<Mutex<AudioLoadState>>) {
    loop {
        let (folder, paths, range, generation) = {
            let mut state = audio_load_state.lock().unwrap();
            let Some(range) = state.requested_range.take() else {
                state.running = false;
                return;
            };
            (
                state.folder.clone(),
                state.paths.clone(),
                range,
                state.generation,
            )
        };
        let end = range.1.min(paths.len());
        for index in range.0.min(end)..end {
            let path = paths[index].clone();
            {
                let state = audio_load_state.lock().unwrap();
                if state.generation != generation || state.generated.contains(&path) {
                    continue;
                }
            }
            let (cache_key, peaks) = waveform::load_or_generate(&path, &folder);
            let mut metadata_index = metadata::load_index(&folder);
            let path_string = path.to_string_lossy().into_owned();
            if let Some(stored) = metadata_index
                .audio_files
                .iter_mut()
                .find(|item| item.file_path == path_string)
            {
                stored.waveform_cache_key = cache_key;
            } else {
                metadata_index
                    .audio_files
                    .push(metadata::AudioFileMetadata {
                        file_path: path_string.clone(),
                        waveform_cache_key: cache_key,
                        ..Default::default()
                    });
            }
            metadata::save_index(&folder, &metadata_index);
            {
                let mut state = audio_load_state.lock().unwrap();
                if state.generation != generation {
                    continue;
                }
                state.generated.insert(path);
                state.completed += 1;
            }
            let state = audio_load_state.lock().unwrap();
            let _ = state.result_sender.send(AudioResult {
                generation,
                index,
                path: path_string,
                peaks,
            });
        }
    }
}

fn refresh_tree(window: &MainWindow, tree_state: &Rc<RefCell<Option<TreeState>>>) {
    let state_ref = tree_state.borrow();
    let Some(state) = state_ref.as_ref() else {
        return;
    };

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

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("adb-studio").join("settings.json"))
}

fn load_settings() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };

    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };

    if fs::create_dir_all(directory).is_err() {
        return;
    }

    let Ok(contents) = serde_json::to_string_pretty(settings) else {
        return;
    };

    let _ = fs::write(path, contents);
}

fn parse_hex_color(value: &str) -> Option<slint::Color> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(slint::Color::from_argb_u8(255, red, green, blue))
}

fn parse_color(value: &str, fallback: slint::Color) -> slint::Color {
    parse_hex_color(value).unwrap_or(fallback)
}
