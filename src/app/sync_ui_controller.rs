use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use slint::{ComponentHandle, Model};

use crate::{
    audio::loader::State as AudioLoadState,
    settings::{self, AppSettings},
    sync::{self, ComfyUiClient, SyncConfig},
    workspace::{
        file_system::TreeState,
        library::refresh_audio,
        tree_nav::{refresh_tree, select_tree_path},
    },
    MainWindow,
};

pub fn register_sync_ui_callbacks(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<slint::VecModel<crate::AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    sync_controller: &Rc<RefCell<crate::sync::SyncController>>,
    recreate_workflow_pending: &Rc<RefCell<bool>>,
    edited_workflow: &Rc<RefCell<Option<serde_json::Value>>>,
) {
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
        window.on_close_tips(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_tips_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        window.on_tips_preference_changed(move |hide_tips| {
            settings.borrow_mut().hide_tips_of_the_day = hide_tips;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_hide_tips_of_the_day(hide_tips);
                if hide_tips {
                    window.set_tips_visible(false);
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        window.on_comfyui_sync_requested(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_error(
                    "Open a workspace before configuring ComfyUI sync".into(),
                );
                window.set_comfyui_sync_visible(true);
                return;
            };
            let config = sync::load_config(&folder).unwrap_or_default();
            window.set_comfyui_sync_url(config.url.into());
            window.set_comfyui_sync_remote_directory(config.remote_directory.into());
            window.set_comfyui_sync_local_directory(config.local_directory.into());
            window.set_comfyui_sync_interval(config.interval_ms.to_string().into());
            window.set_comfyui_sync_error("".into());
            window.set_comfyui_sync_test_message("".into());
            window.set_comfyui_sync_test_success(false);
            window.set_comfyui_sync_visible(true);
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        window.on_comfyui_sync_choose_directory(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                return;
            };
            if let Some(directory) = rfd::FileDialog::new()
                .set_title("Choose ComfyUI download directory")
                .set_directory(&folder)
                .pick_folder()
            {
                if let Ok(relative) = directory.strip_prefix(&folder) {
                    window.set_comfyui_sync_local_directory(
                        relative.to_string_lossy().into_owned().into(),
                    );
                    window.set_comfyui_sync_error("".into());
                    window.set_comfyui_sync_test_message("".into());
                } else {
                    window.set_comfyui_sync_error(
                        "Download directory must be inside the workspace".into(),
                    );
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        window.on_comfyui_sync_test(move |url, remote_directory, local_directory, interval| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(
                    "Open a workspace before testing ComfyUI sync".into(),
                );
                return;
            };
            let Ok(interval_ms) = interval.trim().parse::<u64>() else {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(
                    "Sync frequency must be a number of milliseconds".into(),
                );
                return;
            };
            let config = SyncConfig {
                url: url.to_string(),
                remote_directory: remote_directory.to_string(),
                local_directory: local_directory.to_string(),
                interval_ms,
            }
            .normalized();
            if let Err(error) = config.local_path(&folder) {
                window.set_comfyui_sync_test_success(false);
                window.set_comfyui_sync_test_message(error.to_string().into());
                return;
            }
            match ComfyUiClient::new().and_then(|client| client.list_files(&config)) {
                Ok(files) => {
                    window.set_comfyui_sync_error("".into());
                    window.set_comfyui_sync_test_success(true);
                    window.set_comfyui_sync_test_message(
                        format!("Connection successful: {} file(s) found", files.len()).into(),
                    );
                }
                Err(error) => {
                    window.set_comfyui_sync_test_success(false);
                    window.set_comfyui_sync_test_message(error.to_string().into());
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let audio_folder = Rc::clone(audio_folder);
        let sync_controller = Rc::clone(sync_controller);
        let recreate_workflow_pending = Rc::clone(recreate_workflow_pending);
        let edited_workflow = Rc::clone(edited_workflow);
        window.on_comfyui_sync_save(move |url, remote_directory, local_directory, interval| {
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            let Some(folder) = audio_folder.borrow().clone() else {
                window.set_comfyui_sync_error(
                    "Open a workspace before configuring ComfyUI sync".into(),
                );
                return;
            };
            let Ok(interval_ms) = interval.trim().parse::<u64>() else {
                window.set_comfyui_sync_error(
                    "Sync frequency must be a number of milliseconds".into(),
                );
                return;
            };
            let config = SyncConfig {
                url: url.to_string(),
                remote_directory: remote_directory.to_string(),
                local_directory: local_directory.to_string(),
                interval_ms,
            }
            .normalized();
            if let Err(error) = config.local_path(&folder) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            sync_controller.borrow_mut().stop();
            let test_result =
                ComfyUiClient::new().and_then(|client| client.list_files(&config).map(|_| ()));
            if let Err(error) = test_result {
                window.set_comfyui_sync_active(false);
                window.set_comfyui_sync_error_state(true);
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            if let Err(error) = sync::ensure_destination(&folder, &config) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            if let Err(error) = sync::save_config(&folder, &config) {
                window.set_comfyui_sync_error(error.to_string().into());
                return;
            }
            sync_controller.borrow_mut().start(folder, config);
            window.set_comfyui_sync_error("".into());
            window.set_comfyui_sync_error_state(false);
            window.set_comfyui_sync_visible(false);
            if *recreate_workflow_pending.borrow() {
                *recreate_workflow_pending.borrow_mut() = false;
                if let Some(folder) = audio_folder.borrow().clone() {
                    if let Some(config) = sync::load_config(&folder) {
                        let audio_path = window.get_selected_audio_path().to_string();
                        if !audio_path.is_empty() {
                            let mut workflow = if let Some(workflow) = edited_workflow.borrow().clone() {
                                workflow
                            } else {
                                let Some(workflow_path) = crate::metadata::workflow_path(&folder, std::path::Path::new(&audio_path)) else {
                                    return;
                                };
                                let Ok(contents) = std::fs::read_to_string(workflow_path) else {
                                    return;
                                };
                                let Ok(workflow) = serde_json::from_str(&contents) else {
                                    return;
                                };
                                workflow
                            };
                            for (field, value) in [
                                ("bpm", window.get_workflow_bpm().to_string()),
                                ("key", window.get_workflow_key().to_string()),
                                ("seed", window.get_workflow_seed().to_string()),
                                ("prompt", window.get_workflow_prompt().to_string()),
                                ("lyrics", window.get_workflow_lyrics().to_string()),
                            ] {
                                crate::metadata::comfyui::update_metadata(&mut workflow, field, &value);
                            }
                            let _ = ComfyUiClient::new().and_then(|client| client.upload_workflow(&config, &workflow));
                        }
                    }
                }
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_comfyui_sync_cancel(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_comfyui_sync_visible(false);
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
        let settings = Rc::clone(settings);
        window.on_theme_selected(move |light_theme| {
            settings.borrow_mut().light_theme = light_theme;
            let settings_snapshot = settings.borrow().clone();
            settings::save(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_theme_index(if light_theme { 1 } else { 0 });
                window.set_light_theme(light_theme);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(settings);
        let tree_state = Rc::clone(tree_state);
        let audio_folder = Rc::clone(audio_folder);
        let audio_model = Rc::clone(audio_model);
        let audio_load_state = Arc::clone(audio_load_state);
        window.on_sort_order_selected(move |sort_order| {
            settings.borrow_mut().sort_order = sort_order;
            settings::save(&settings.borrow());
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            refresh_tree(&window, &tree_state);
            let Some(workspace) = audio_folder.borrow().clone() else {
                return;
            };
            let folder = settings
                .borrow()
                .last_selected_path
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.exists() && path.strip_prefix(&workspace).is_ok())
                .map(|path| {
                    if path.is_dir() {
                        path
                    } else {
                        path.parent().unwrap_or(&workspace).to_path_buf()
                    }
                })
                .unwrap_or(workspace);
            refresh_audio(
                &window,
                &audio_folder,
                &audio_model,
                &audio_load_state,
                folder,
            );
        });
    }
}
