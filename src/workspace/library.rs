use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{atomic::Ordering, Arc, Mutex},
};

use slint::{Model, ModelRc, VecModel};

use crate::{
    audio::{loader, view::comment_rows, waveform},
    metadata,
    workspace::{file_system, preferences},
    AudioLoadState, AudioRow, MainWindow, TrackDifference,
};

use super::{pinned_track_sort, tree_nav::set_audio_breadcrumbs};

pub fn refresh_audio(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
) {
    refresh_audio_with_changes(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        None,
    );
}

pub fn refresh_audio_for_changes(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: &[PathBuf],
) {
    refresh_audio_with_changes(
        window,
        audio_folder,
        audio_model,
        audio_load_state,
        folder,
        Some(changed_paths),
    );
}

fn refresh_audio_with_changes(
    window: &MainWindow,
    audio_folder: &Rc<RefCell<Option<PathBuf>>>,
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    audio_load_state: &Arc<Mutex<AudioLoadState>>,
    folder: PathBuf,
    changed_paths: Option<&[PathBuf]>,
) {
    if let Some(workspace) = audio_folder.borrow().clone() {
        set_audio_breadcrumbs(window, &workspace, &folder);
    }
    let filter = window.get_audio_filter().to_string();
    let reload_all = changed_paths.is_none();
    let changed_audio_paths = changed_paths
        .into_iter()
        .flat_map(|paths| paths.iter())
        .filter(|path| path.parent() == Some(folder.as_path()))
        .filter(|path| file_system::FileKind::from_path(path) == file_system::FileKind::Audio)
        .cloned()
        .collect::<HashSet<_>>();
    let existing_rows = audio_model
        .borrow()
        .clone()
        .map(|model| {
            (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .map(|row| (PathBuf::from(row.path.as_str()), row))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let generated_paths = audio_load_state
        .lock()
        .unwrap()
        .generated
        .clone();
    let pinned_path = audio_folder
        .borrow()
        .clone()
        .and_then(|workspace| preferences::pinned_track(&workspace, &folder));
    let workspace = audio_folder.borrow().clone().unwrap_or_default();
    let sort_order = file_system::SortOrder::from_i32(window.get_sort_order());
    let mut entries = file_system::read_dir_sorted(&folder, sort_order);
    pinned_track_sort::sort_tracks(&folder, pinned_path.as_deref(), sort_order, &mut entries);
    let mut rows = Vec::new();
    let mut preserved_waveform_paths = HashSet::new();
    for entry in entries {
        if entry.kind != file_system::FileKind::Audio
            || !file_system::matches_audio_filter(&entry.name, &filter)
        {
            continue;
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let existing_row = existing_rows.get(&entry.path);
        let can_reuse_waveform = existing_row.is_some_and(|row| {
            generated_paths.contains(&entry.path)
                && row.modified_date == modified_date.to_string()
        });
        let should_reload = changed_audio_paths.contains(&entry.path)
            || (reload_all && !can_reuse_waveform);
        if !should_reload && !reload_all {
            if let Some(row) = existing_rows.get(&entry.path) {
                rows.push(row.clone());
                continue;
            }
        }
        let path_string = entry.path.to_string_lossy().into_owned();
        let audio_metadata = metadata::load_audio_metadata(&workspace, &entry.path);
        let comments = comment_rows(&audio_metadata);
        let progress = (audio_metadata.duration_seconds > 0.0)
            .then_some(audio_metadata.last_position_seconds / audio_metadata.duration_seconds)
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let differences = pinned_path
            .as_deref()
            .map(|pinned| metadata::comfyui::compare_files(&folder, pinned, &entry.path))
            .unwrap_or_default();
        let similarity = pinned_path
            .as_deref()
            .map(|_| pinned_track_sort::similarity_from_differences(&differences))
            .unwrap_or(-1.0);
        rows.push(AudioRow {
            path: path_string.into(),
            name: entry.name.into(),
            modified_date: modified_date.to_string().into(),
            peaks: if can_reuse_waveform {
                preserved_waveform_paths.insert(entry.path.clone());
                existing_row
                    .map(|row| row.peaks.clone())
                    .unwrap_or_else(|| {
                        ModelRc::new(VecModel::from(vec![
                            0.0;
                            waveform::DISPLAY_PEAK_COUNT
                        ]))
                    })
            } else {
                ModelRc::new(VecModel::from(vec![0.0; waveform::DISPLAY_PEAK_COUNT]))
            },
            is_loading: false,
            comments,
            differences: ModelRc::new(VecModel::from(
                differences
                    .into_iter()
                    .map(|difference| TrackDifference {
                        label: difference.label.into(),
                        value: difference.value.into(),
                    })
                    .collect::<Vec<_>>(),
            )),
            similarity,
            rating: audio_metadata.normalized_rating() as i32,
            is_pinned: pinned_path.as_deref() == Some(entry.path.as_path()),
            is_active: false,
            is_selected: false,
            is_primary: false,
            is_playing: false,
            progress,
            loop_enabled: false,
            selected_comment_start: -1.0,
            selected_comment_end: -1.0,
        });
    }
    let previous_selected_path = PathBuf::from(window.get_selected_audio_path().as_str());
    let selected_paths = window
        .get_tree_rows()
        .iter()
        .filter(|row| row.is_selected)
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect::<HashSet<_>>();
    let selected_path = rows
        .iter()
        .find(|row| Path::new(row.path.as_str()) == previous_selected_path)
        .map(|row| row.path.clone())
        .or_else(|| rows.first().map(|row| row.path.clone()))
        .unwrap_or_default();
    for row in &mut rows {
        let path = Path::new(row.path.as_str());
        row.is_selected = selected_paths.contains(path) || row.path == selected_path;
        row.is_primary = row.path == selected_path;
    }
    window.set_selected_audio_path(selected_path);
    let paths: Vec<PathBuf> = rows
        .iter()
        .map(|row| PathBuf::from(row.path.as_str()))
        .collect();
    let total = paths.len();
    let model = Rc::new(VecModel::from(rows));
    window.set_audio_rows(ModelRc::new(model.clone()));
    *audio_model.borrow_mut() = Some(model);
    {
        let mut state = audio_load_state.lock().unwrap();
        state.folder = audio_folder.borrow().clone().unwrap_or_default();
        state.paths = paths;
        if reload_all {
            state
                .generated
                .retain(|path| preserved_waveform_paths.contains(path));
            state.loading.clear();
        } else {
            let current_paths = state.paths.iter().cloned().collect::<HashSet<_>>();
            state
                .generated
                .retain(|path| current_paths.contains(path) && !changed_audio_paths.contains(path));
            state
                .loading
                .retain(|path| current_paths.contains(path) && !changed_audio_paths.contains(path));
        }
        state.generation += 1;
        state
            .cancellation_generation
            .store(state.generation, Ordering::Release);
        state.completed = state.generated.len();
        state.total = total;
        state.requested_range = None;
    }
    loader::request(audio_load_state, 0, 1);
}

