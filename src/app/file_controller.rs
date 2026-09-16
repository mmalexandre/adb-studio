use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};

use slint::{ModelRc, SharedString, VecModel};

use crate::{
    app::FileClipboard,
    audio::loader::State as AudioLoadState,
    settings::{self, AppSettings},
    workspace::{
        file_system::{self, TreeState},
        library::refresh_audio,
        tree_nav::refresh_tree,
    },
    AudioRow, MainWindow,
};

pub fn set_cut_paths(window: &MainWindow, paths: &[PathBuf]) {
    let paths = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned().into())
        .collect::<Vec<SharedString>>();
    window.set_cut_paths(ModelRc::new(VecModel::from(paths)));
}

pub fn selected_sources(
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    source: &Path,
) -> Vec<PathBuf> {
    let selected = tree_state
        .borrow()
        .as_ref()
        .map(TreeState::selected_paths)
        .unwrap_or_default();
    if selected.iter().any(|path| path == source) {
        selected
    } else {
        vec![source.to_path_buf()]
    }
}

pub fn move_sources_to_directory(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    sources: &[PathBuf],
    target: &Path,
) -> bool {
    let destinations = match file_system::move_paths(sources, target) {
        Ok(destinations) => destinations,
        Err(error) => {
            window.set_audio_error(format!("File operation: {error}").into());
            return false;
        }
    };
    for (source, destination) in sources.iter().zip(&destinations) {
        if let Some(folder) = audio_folder.borrow().as_ref() {
            if let Err(error) =
                crate::metadata::rename_associated_workflow(folder, source, destination)
            {
                window.set_audio_error(format!("Workflow file operation: {error}").into());
            }
            crate::metadata::rename_audio_metadata(folder, source, destination);
        }
    }
    finish_operation(
        window,
        settings,
        tree_state,
        audio_folder,
        audio_model,
        audio_load_state,
        destinations,
    );
    true
}

pub fn copy_sources_to_directory(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    sources: &[PathBuf],
    target: &Path,
) -> bool {
    let destinations = match file_system::copy_paths(sources, target) {
        Ok(destinations) => destinations,
        Err(error) => {
            window.set_audio_error(format!("File operation: {error}").into());
            return false;
        }
    };
    finish_operation(
        window,
        settings,
        tree_state,
        audio_folder,
        audio_model,
        audio_load_state,
        destinations,
    );
    true
}

pub fn handle_clipboard_action(
    window: &MainWindow,
    action: i32,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    clipboard: &Rc<RefCell<Option<FileClipboard>>>,
) {
    match action {
        0 | 1 => {
            let sources = tree_state
                .borrow()
                .as_ref()
                .map(TreeState::selected_paths)
                .unwrap_or_default()
                .into_iter()
                .filter(|path| path.exists())
                .collect::<Vec<_>>();
            if sources.is_empty() {
                return;
            }
            let cut = action == 1;
            *clipboard.borrow_mut() = Some(FileClipboard {
                paths: sources.clone(),
                cut,
            });
            let cut_paths = if cut { sources.as_slice() } else { &[] };
            set_cut_paths(window, cut_paths);
            refresh_tree(window, tree_state);
            crate::audio::view::update_audio_cut_rows(audio_model, cut_paths);
        }
        2 => {
            let Some(operation) = clipboard.borrow().clone() else {
                return;
            };
            let Some(target) = selected_target(tree_state, audio_folder) else {
                return;
            };
            let succeeded = if operation.cut {
                move_sources_to_directory(
                    window,
                    settings,
                    tree_state,
                    audio_folder,
                    audio_model,
                    audio_load_state,
                    &operation.paths,
                    &target,
                )
            } else {
                copy_sources_to_directory(
                    window,
                    settings,
                    tree_state,
                    audio_folder,
                    audio_model,
                    audio_load_state,
                    &operation.paths,
                    &target,
                )
            };
            if succeeded && operation.cut {
                *clipboard.borrow_mut() = None;
                set_cut_paths(window, &[]);
                refresh_tree(window, tree_state);
                crate::audio::view::update_audio_cut_rows(audio_model, &[]);
            }
        }
        _ => {}
    }
}

fn selected_target(
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
) -> Option<PathBuf> {
    tree_state
        .borrow()
        .as_ref()
        .and_then(|state| state.selected.clone())
        .and_then(|path| {
            if path.is_dir() {
                Some(path)
            } else {
                path.parent().map(Path::to_path_buf)
            }
        })
        .or_else(|| audio_folder.borrow().clone())
}

fn finish_operation(
    window: &MainWindow,
    settings: &Rc<RefCell<AppSettings>>,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    destinations: Vec<PathBuf>,
) {
    let Some(primary) = destinations.last().cloned() else {
        return;
    };
    {
        let mut state_ref = tree_state.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return;
        };
        state.select_paths(destinations, primary.clone());
    }
    settings.borrow_mut().last_selected_path = Some(primary.to_string_lossy().into_owned());
    settings::save(&settings.borrow());
    window.set_selected_name(
        primary
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    window.set_audio_error("".into());
    refresh_views(
        window,
        tree_state,
        audio_folder,
        audio_model,
        audio_load_state,
    );
}

fn refresh_views(
    window: &MainWindow,
    tree_state: &Rc<RefCell<Option<TreeState>>>,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
) {
    refresh_tree(window, tree_state);
    if let Some(folder) = audio_folder.borrow().clone() {
        refresh_audio(window, audio_folder, audio_model, audio_load_state, folder);
    }
}
