use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};

use slint::{ComponentHandle, Model, SharedString};

use crate::{
    audio::{
        comment_extraction,
        loader::State as AudioLoadState,
        playback::PlaybackEngine,
        session::comment_duration,
        view::{
            available_tag_rows, comment_rows_with_labels, label_row_fields, select_comment,
            tag_rows, update_audio_subtitle, update_comment_model, user_comment_subtitle,
        },
    },
    metadata::{self, AudioComment},
    settings::{self, AppSettings},
    workspace::{file_system::TreeState, library::refresh_audio, tree_nav::select_tree_path},
    MainWindow,
};

/// Wires comment editing/colors, ratings, trash confirmation, and pane sizing callbacks.
#[allow(clippy::too_many_arguments)]
pub fn register_metadata_pane_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    playback: &Rc<RefCell<Option<PlaybackEngine>>>,
    comment_editor_original: &Rc<RefCell<Option<AudioComment>>>,
    comment_editor_duration: &Rc<RefCell<f32>>,
) {
    {
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let settings = Rc::clone(settings);
        window.on_audio_tag_add_requested(move |path, tag_id| {
            update_audio_tag_assignment(
                &audio_folder,
                &audio_model,
                &settings,
                path.as_str(),
                tag_id,
                true,
            );
        });
    }

    {
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let settings = Rc::clone(settings);
        window.on_audio_tag_remove_requested(move |path, tag_id| {
            update_audio_tag_assignment(
                &audio_folder,
                &audio_model,
                &settings,
                path.as_str(),
                tag_id,
                false,
            );
        });
    }

    {
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let settings = Rc::clone(settings);
        window.on_audio_label_selected(move |path, selected_label_id| {
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let audio_path = PathBuf::from(path.as_str());
            let mut file = metadata::load_audio_metadata(&folder, &audio_path);
            file.label_id = (selected_label_id >= 0).then_some(selected_label_id as u64);
            metadata::save_audio_metadata(&folder, &audio_path, &file);
            let (label_id, label_name, label_color, label_known) =
                label_row_fields(file.label_id, &settings.borrow().label_definitions);
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    if row.path == path {
                        model.set_row_data(
                            index,
                            crate::AudioRow {
                                path: row.path,
                                name: row.name,
                                subtitle: row.subtitle,
                                custom_tag: row.custom_tag,
                                is_folder: row.is_folder,
                                is_lora: row.is_lora,
                                depth: row.depth,
                                is_expanded: row.is_expanded,
                                modified_date: row.modified_date,
                                waveform: row.waveform,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: row.differences,
                                similarity: row.similarity,
                                rating: row.rating,
                                is_pinned: row.is_pinned,
                                is_selected: row.is_selected,
                                is_cut: row.is_cut,
                                is_primary: row.is_primary,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                duration_seconds: row.duration_seconds,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                                label_id,
                                label_name,
                                label_color,
                                label_known,
                                tags: row.tags,
                                available_tags: row.available_tags,
                                detected_bpm: row.detected_bpm,
                            },
                        );
                        break;
                    }
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        window.on_user_comments_changed(move |comments| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(window.get_selected_audio_path().as_str());
            if path.as_os_str().is_empty() {
                return;
            }
            let mut file = metadata::load_audio_metadata(&folder, &path);
            file.file_path = path.to_string_lossy().into_owned();
            file.user_comments = comments.to_string();
            let subtitle = user_comment_subtitle(&file.user_comments);
            metadata::save_audio_metadata(&folder, &path, &file);
            update_audio_subtitle(&audio_model, &path, &subtitle);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        window.on_lora_custom_path_changed(move |path_value| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(window.get_selected_audio_path().as_str());
            if path.as_os_str().is_empty() {
                return;
            }
            metadata::save_lora_custom_path(
                &folder,
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
                path_value.as_str(),
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        window.on_lora_custom_tag_changed(move |tag| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(window.get_selected_audio_path().as_str());
            if path.as_os_str().is_empty() {
                return;
            }
            metadata::save_lora_custom_tag(
                &folder,
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
                tag.as_str(),
            );
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    if row.path == path.to_string_lossy().as_ref() {
                        let mut updated = row.clone();
                        updated.custom_tag = tag.clone();
                        model.set_row_data(index, updated);
                        break;
                    }
                }
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
            let rating = rating.clamp(0, 5) as u8;
            let audio_path = PathBuf::from(path.as_str());
            let mut file = metadata::load_audio_metadata(&folder, &audio_path);
            file.rating = rating;
            metadata::save_audio_metadata(&folder, &audio_path, &file);
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
                                subtitle: row.subtitle,
                                custom_tag: row.custom_tag,
                                is_folder: row.is_folder,
                                is_lora: row.is_lora,
                                depth: row.depth,
                                is_expanded: row.is_expanded,
                                modified_date: row.modified_date,
                                waveform: row.waveform,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: row.differences,
                                similarity: row.similarity,
                                rating: rating as i32,
                                is_pinned: row.is_pinned,
                                is_selected: row.is_selected,
                                is_cut: row.is_cut,
                                is_primary: row.is_primary,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                duration_seconds: row.duration_seconds,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                                label_id: row.label_id,
                                label_name: row.label_name,
                                label_color: row.label_color,
                                label_known: row.label_known,
                                tags: row.tags,
                                available_tags: row.available_tags,
                                detected_bpm: row.detected_bpm,
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
        let settings = Rc::clone(settings);
        window.on_comment_range_moved(move |path, old_start, old_end, start, end, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
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
            let (start, end) = metadata::quantize_comment_range(
                start,
                end,
                duration,
                window.get_workflow_bpm().as_str(),
                window.get_comment_quantization_index(),
            );
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
            let comments = comment_rows_with_labels(&file, &settings.borrow().label_definitions);
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
            if let Err(error) = trash::delete(&source) {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            if playback_for_move
                .borrow()
                .as_ref()
                .and_then(|engine| engine.path())
                == Some(source.as_path())
            {
                if let Some(engine) = playback_for_move.borrow_mut().as_mut() {
                    engine.stop();
                }
                window.set_active_audio_path("".into());
                window.set_audio_file_name("".into());
                window.set_audio_playing(false);
            }
            let parent = source.parent().unwrap_or(&folder).to_path_buf();
            select_tree_path(&window, &tree_state_for_move, &settings_for_move, &parent);
            refresh_audio(
                &window,
                &audio_folder_for_move,
                &audio_model_for_move,
                &audio_load_state_for_move,
                parent,
            );
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
            if !settings_for_request
                .borrow()
                .trash_confirmation_disabled_workspaces
                .contains(&folder.to_string_lossy().to_string())
            {
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
                let last_folder = settings.borrow().last_folder.clone();
                if let Some(folder) = last_folder {
                    settings
                        .borrow_mut()
                        .trash_confirmation_disabled_workspaces
                        .insert(folder);
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
            let mut original = AudioComment {
                start_seconds: start * duration,
                end_seconds: end * duration,
                text: text.to_string(),
                label_id: None,
            };
            let stored_metadata = metadata::load_audio_metadata(&folder, &path);
            let label_id = stored_metadata
                .comments
                .iter()
                .find(|comment| {
                    (comment.start_seconds / duration - start).abs() < 0.001
                        && (comment.end_seconds / duration - end).abs() < 0.001
                        && comment.text == text.as_str()
                })
                .and_then(|comment| comment.label_id)
                .map(|id| id as i32)
                .unwrap_or(-1);
            original.label_id = (label_id >= 0).then_some(label_id as u64);
            *comment_editor_duration.borrow_mut() = duration;
            select_comment(&audio_model, &path, start, end);
            *comment_editor_original.borrow_mut() = Some(original.clone());
            window.set_comment_editor_path(path.to_string_lossy().into_owned().into());
            window.set_comment_editor_text(original.text.into());
            window.set_comment_editor_label_id(label_id);
            window
                .set_comment_editor_extract_enabled(original.end_seconds > original.start_seconds);
            window.set_comment_editor_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let comment_editor_original = Rc::clone(comment_editor_original);
        window.on_comment_extract_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(_workspace) = audio_folder.borrow().clone() else {
                window.set_audio_error("Open a workspace before extracting audio".into());
                return;
            };
            let Some(comment) = comment_editor_original.borrow().clone() else {
                window.set_audio_error("Unable to determine comment range".into());
                return;
            };
            let source = PathBuf::from(path.as_str());
            let destination = match comment_extraction::destination(
                &source,
                comment.start_seconds,
                comment.end_seconds,
            ) {
                Ok(destination) => destination,
                Err(error) => {
                    window.set_audio_error(error.into());
                    return;
                }
            };
            let start_seconds = comment.start_seconds;
            let end_seconds = comment.end_seconds;
            window.set_audio_error("Extracting audio...".into());
            std::thread::spawn(move || {
                let _ =
                    comment_extraction::extract(&source, &destination, start_seconds, end_seconds);
            });
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let comment_editor_duration = Rc::clone(comment_editor_duration);
        let settings = Rc::clone(settings);
        window.on_comment_save(move |path, text| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = audio_folder.borrow().clone() else {
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
            let original = comment_editor_original.borrow().clone();
            let Some(original) = original else {
                window.set_audio_error("Unable to determine comment range".into());
                return;
            };
            let (start_seconds, end_seconds) = (original.start_seconds, original.end_seconds);
            let comment = AudioComment {
                start_seconds,
                end_seconds,
                text: text.to_string(),
                label_id: (window.get_comment_editor_label_id() >= 0)
                    .then_some(window.get_comment_editor_label_id() as u64),
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
            if let Err(error) = metadata::save_audio_metadata_checked(&folder, &path, &file) {
                window.set_audio_error(format!("Save comment: {error}").into());
                return;
            }
            update_comment_model(
                &audio_model,
                &path,
                comment_rows_with_labels(&file, &settings.borrow().label_definitions),
            );
            select_comment(&audio_model, &path, selected_start, selected_end);
            window.set_comment_editor_visible(false);
            window.set_audio_error("Comment saved".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let comment_editor_original = Rc::clone(comment_editor_original);
        let settings = Rc::clone(settings);
        window.on_comment_delete(move |path| {
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
                if let Err(error) = metadata::save_audio_metadata_checked(&workspace, &path, &file)
                {
                    window.set_audio_error(format!("Delete comment: {error}").into());
                    return;
                }
                update_comment_model(
                    &audio_model,
                    &path,
                    comment_rows_with_labels(&file, &settings.borrow().label_definitions),
                );
            }
            window.set_comment_editor_visible(false);
            window.set_audio_error("Comment deleted".into());
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

    {
        let settings = Rc::clone(settings);
        window.on_metadata_first_column_width_changed(move |width| {
            settings.borrow_mut().metadata_first_column_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(settings);
        window.on_metadata_second_column_width_changed(move |width| {
            settings.borrow_mut().metadata_second_column_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }

    {
        let settings = Rc::clone(settings);
        window.on_metadata_right_column_width_changed(move |width| {
            settings.borrow_mut().metadata_right_column_width = width;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
        });
    }
}

fn update_audio_tag_assignment(
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    settings: &Rc<RefCell<AppSettings>>,
    path: &str,
    tag_id: i32,
    add: bool,
) {
    let Some(folder) = audio_folder.borrow().clone() else {
        return;
    };
    let audio_path = PathBuf::from(path);
    let mut file = metadata::load_audio_metadata(&folder, &audio_path);
    let tag_id = tag_id as u64;
    if add {
        if !file.tag_ids.contains(&tag_id) {
            file.tag_ids.push(tag_id);
        }
    } else {
        file.tag_ids.retain(|id| *id != tag_id);
    }
    metadata::save_audio_metadata(&folder, &audio_path, &file);
    let settings = settings.borrow();
    let tags = tag_rows(&file.tag_ids, &settings.tag_definitions);
    let available_tags = available_tag_rows(&file.tag_ids, &settings.tag_definitions);
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        if row.path.as_str() != path {
            continue;
        }
        model.set_row_data(
            index,
            crate::AudioRow {
                path: row.path,
                name: row.name,
                subtitle: row.subtitle,
                custom_tag: row.custom_tag,
                is_folder: row.is_folder,
                is_lora: row.is_lora,
                depth: row.depth,
                is_expanded: row.is_expanded,
                modified_date: row.modified_date,
                waveform: row.waveform,
                is_loading: row.is_loading,
                comments: row.comments,
                differences: row.differences,
                similarity: row.similarity,
                rating: row.rating,
                is_pinned: row.is_pinned,
                is_selected: row.is_selected,
                is_cut: row.is_cut,
                is_primary: row.is_primary,
                is_active: row.is_active,
                is_playing: row.is_playing,
                progress: row.progress,
                duration_seconds: row.duration_seconds,
                loop_enabled: row.loop_enabled,
                selected_comment_start: row.selected_comment_start,
                selected_comment_end: row.selected_comment_end,
                label_id: row.label_id,
                label_name: row.label_name,
                label_color: row.label_color,
                label_known: row.label_known,
                tags,
                available_tags,
                detected_bpm: row.detected_bpm,
            },
        );
        break;
    }
}
