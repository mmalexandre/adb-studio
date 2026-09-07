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

use super::tree_nav::set_audio_breadcrumbs;

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
    let pinned_path = audio_folder
        .borrow()
        .clone()
        .and_then(|workspace| preferences::pinned_track(&workspace, &folder));
    let workspace = audio_folder.borrow().clone().unwrap_or_default();
    let index = metadata::load_index(&folder);
    let mut rows = Vec::new();
    for entry in file_system::read_dir_sorted(
        &folder,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    ) {
        if entry.kind != file_system::FileKind::Audio || !matches_audio_filter(&entry.name, &filter)
        {
            continue;
        }
        let should_reload = reload_all || changed_audio_paths.contains(&entry.path);
        if !should_reload {
            if let Some(row) = existing_rows.get(&entry.path) {
                rows.push(row.clone());
                continue;
            }
        }
        let modified_date = fs::metadata(&entry.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let path_string = entry.path.to_string_lossy().into_owned();
        let stored_position = index
            .audio_files
            .iter()
            .find(|item| item.file_path == path_string);
        let comments = comment_rows(&metadata::load_audio_metadata(&workspace, &entry.path));
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
            is_loading: false,
            comments,
            differences: track_differences(&workspace, pinned_path.as_deref(), &entry.path),
            rating: stored_position
                .as_ref()
                .map(|item| item.normalized_rating() as i32)
                .unwrap_or(0),
            is_pinned: pinned_path.as_deref() == Some(entry.path.as_path()),
            is_active: false,
            is_selected: false,
            is_playing: false,
            progress,
            loop_enabled: false,
            selected_comment_start: -1.0,
            selected_comment_end: -1.0,
        });
    }
    let sort_order = file_system::SortOrder::from_i32(window.get_sort_order());
    rows.sort_by(|left, right| match sort_order {
        file_system::SortOrder::AlphabeticalAscending => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
        file_system::SortOrder::AlphabeticalDescending => right
            .name
            .to_ascii_lowercase()
            .cmp(&left.name.to_ascii_lowercase()),
        file_system::SortOrder::ModifiedAscending => left
            .modified_date
            .parse::<u64>()
            .unwrap_or_default()
            .cmp(&right.modified_date.parse::<u64>().unwrap_or_default())
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase())),
        file_system::SortOrder::ModifiedDescending => right
            .modified_date
            .parse::<u64>()
            .unwrap_or_default()
            .cmp(&left.modified_date.parse::<u64>().unwrap_or_default())
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase())),
    });
    let previous_selected_path = PathBuf::from(window.get_selected_audio_path().as_str());
    let selected_path = rows
        .iter()
        .find(|row| Path::new(row.path.as_str()) == previous_selected_path)
        .map(|row| row.path.clone())
        .or_else(|| rows.first().map(|row| row.path.clone()))
        .unwrap_or_default();
    for row in &mut rows {
        row.is_selected = row.path == selected_path;
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
            state.generated.clear();
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

pub fn track_differences(
    folder: &Path,
    pinned_path: Option<&Path>,
    track_path: &Path,
) -> ModelRc<TrackDifference> {
    let differences = pinned_path
        .map(|pinned_path| metadata::comfyui::compare_files(folder, pinned_path, track_path))
        .unwrap_or_default()
        .into_iter()
        .map(|difference| TrackDifference {
            label: difference.label.into(),
            value: difference.value.into(),
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(differences))
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
