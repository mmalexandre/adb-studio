use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc, sync::Mutex};

use slint::{ComponentHandle, Model, VecModel};

use crate::{
    audio::loader::State as AudioLoadState,
    settings::{self, AppSettings},
    workspace::{
        file_system::{self, TreeState},
        library::{refresh_audio, track_differences},
        tree_nav::{refresh_tree, select_tree_path},
        workflow::load_workflow_for_audio,
    },
    AudioRow, MainWindow,
};

/// Wires tree navigation, file rename/create/move, and audio pin/trash callbacks.
#[allow(clippy::too_many_arguments)]
pub fn register_tree_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
    workflow_loading: &Rc<RefCell<bool>>,
) {
    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let audio_model = Rc::clone(audio_model);
        let audio_folder = Rc::clone(audio_folder);
        let edited_workflow = Rc::clone(edited_workflow);
        let workflow_loading = Rc::clone(workflow_loading);
        window.on_audio_row_selected(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            window.set_selected_audio_path(path.clone());
            let path = std::path::Path::new(path.as_str());
            *edited_workflow.borrow_mut() = None;
            window.set_workflow_modified(false);
            crate::audio::view::select_audio_path(&audio_model, path);
            select_tree_path(&window, &tree_state, &settings, path);
            if let Some(folder) = audio_folder.borrow().clone() {
                load_workflow_for_audio(&window, &folder, path, &workflow_loading);
            }
        });
    }

    window.on_tree_reveal_requested(move |path| {
        let path = PathBuf::from(path.as_str());
        if path.exists() {
            let _ = opener::reveal(path);
        }
    });

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(tree_state);
        window.on_chevron_clicked(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let mut state_ref = tree_state.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return;
            };
            state.toggle(&path);
            drop(state_ref);
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        window.on_breadcrumb_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            if !path.is_dir() {
                return;
            }
            select_tree_path(&window, &tree_state, &settings, &path);
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                path,
            );
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        window.on_tree_trash_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let source = PathBuf::from(path.as_str());
            if !source.exists() {
                return;
            }
            if let Err(error) = trash::delete(&source) {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            let selection_removed = {
                let mut state_ref = tree_state.borrow_mut();
                let Some(state) = state_ref.as_mut() else {
                    return;
                };
                state.remove_path(&source)
            };
            if selection_removed {
                settings.borrow_mut().last_selected_path = None;
                let settings_snapshot = settings.borrow().clone();
                settings::save(&settings_snapshot);
                window.set_selected_name("".into());
            }
            window.set_audio_error("".into());
            refresh_tree(&window, &tree_state);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        window.on_pin_requested(move |path| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let path = PathBuf::from(path.as_str());
            let Some(folder) = path.parent() else {
                return;
            };
            let already_pinned = audio_model
                .borrow()
                .as_ref()
                .and_then(|model| {
                    (0..model.row_count())
                        .filter_map(|index| model.row_data(index))
                        .find(|row| std::path::Path::new(row.path.as_str()) == path)
                })
                .is_some_and(|row| row.is_pinned);
            crate::workspace::preferences::set_pinned_track(
                &workspace,
                folder,
                (!already_pinned).then_some(path.as_path()),
            );
            let pinned_path = (!already_pinned).then_some(path.as_path());
            if let Some(model) = audio_model.borrow().clone() {
                for index in 0..model.row_count() {
                    let Some(row) = model.row_data(index) else {
                        continue;
                    };
                    let is_pinned =
                        !already_pinned && std::path::Path::new(row.path.as_str()) == path;
                    if row.is_pinned != is_pinned {
                        model.set_row_data(
                            index,
                            AudioRow {
                                path: row.path.clone(),
                                name: row.name,
                                modified_date: row.modified_date,
                                peaks: row.peaks,
                                is_loading: row.is_loading,
                                comments: row.comments,
                                differences: track_differences(
                                    &workspace,
                                    pinned_path,
                                    std::path::Path::new(row.path.as_str()),
                                ),
                                rating: row.rating,
                                is_pinned,
                                is_selected: row.is_selected,
                                is_active: row.is_active,
                                is_playing: row.is_playing,
                                progress: row.progress,
                                loop_enabled: row.loop_enabled,
                                selected_comment_start: row.selected_comment_start,
                                selected_comment_end: row.selected_comment_end,
                            },
                        );
                    }
                }
            }
            window.set_audio_error("".into());
        });
    }

    {
        let weak_window = window.as_weak();
        let tree_state = Rc::clone(tree_state);
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
        let tree_state = Rc::clone(tree_state);
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
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let audio_folder = Rc::clone(audio_folder);
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
                std::fs::create_dir(&destination)
            } else {
                std::fs::rename(&source, &destination)
            };
            if let Err(error) = result {
                window.set_audio_error(format!("File operation: {error}").into());
                return;
            }
            if mode != 2 {
                if let Some(folder) = audio_folder.borrow().as_ref() {
                    if let Err(error) =
                        crate::metadata::rename_associated_workflow(folder, &source, &destination)
                    {
                        window.set_audio_error(format!("Workflow file operation: {error}").into());
                    }
                }
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
        let tree_state = Rc::clone(tree_state);
        let settings = Rc::clone(settings);
        let audio_folder = Rc::clone(audio_folder);
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
                let rows = file_system::build_visible_rows(
                    state,
                    file_system::SortOrder::from_i32(window.get_sort_order()),
                );
                let target_index = (source_index as f32 + ((pointer_y - 13.0) / 26.0).round())
                    .clamp(0.0, (rows.len() - 1) as f32) as usize;
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
                    window
                        .set_audio_error("File operation: cannot move a folder into itself".into());
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
                if let Err(error) = std::fs::rename(source, destination) {
                    window.set_audio_error(format!("File operation: {error}").into());
                    return;
                }
                if let Some(folder) = audio_folder.borrow().as_ref() {
                    if let Err(error) =
                        crate::metadata::rename_associated_workflow(folder, source, destination)
                    {
                        window.set_audio_error(format!("Workflow file operation: {error}").into());
                    }
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
}
