use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use slint::{Model, ModelRc, VecModel};

use crate::metadata;
use crate::settings::{self, LabelDefinition, TagDefinition};
use crate::{AudioRow, CommentRow, TagRow};

thread_local! {
    static AUDIO_ROW_INDEX: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new());
    static ACTIVE_AUDIO_PATH: RefCell<Option<PathBuf>> = RefCell::new(None);
}

pub fn set_audio_row_index(model: &VecModel<AudioRow>) {
    AUDIO_ROW_INDEX.with(|index| {
        let mut index = index.borrow_mut();
        index.clear();
        for row_index in 0..model.row_count() {
            if let Some(row) = model.row_data(row_index) {
                index.insert(PathBuf::from(row.path.as_str()), row_index);
            }
        }
    });
}

pub fn row_index(path: &Path) -> Option<usize> {
    AUDIO_ROW_INDEX.with(|index| index.borrow().get(path).copied())
}

pub fn comment_rows_with_labels(
    item: &metadata::AudioFileMetadata,
    labels: &[LabelDefinition],
) -> ModelRc<CommentRow> {
    let duration = item.duration_seconds.max(0.001);
    let mut normalized: Vec<(f32, f32, String, Option<u64>)> = item
        .comments
        .iter()
        .map(|comment| {
            (
                (comment.start_seconds / duration).clamp(0.0, 1.0),
                (comment.end_seconds / duration).clamp(0.0, 1.0),
                comment.text.clone(),
                comment.label_id,
            )
        })
        .collect();
    normalized.sort_by(|left, right| left.0.total_cmp(&right.0));
    let rows = normalized
        .iter()
        .enumerate()
        .map(|(index, (start, end, text, label_id))| {
            let label = label_id.and_then(|id| labels.iter().find(|label| label.id == id));
            CommentRow {
                start: *start,
                end: *end,
                bubble_end: normalized
                    .get(index + 1)
                    .map(|next| next.0)
                    .unwrap_or(1.0)
                    .max(*start),
                text: text.clone().into(),
                label_id: label_id.map(|id| id as i32).unwrap_or(-1),
                label_color: label
                    .map(|label| settings::parse_color(&label.color, fallback_label_color()))
                    .unwrap_or_else(fallback_label_color),
                label_known: label.is_some(),
            }
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(rows))
}

fn fallback_label_color() -> slint::Color {
    slint::Color::from_argb_u8(255, 229, 138, 77)
}

pub fn label_row_fields(
    label_id: Option<u64>,
    labels: &[LabelDefinition],
) -> (i32, slint::SharedString, slint::Color, bool) {
    let Some(label_id) = label_id else {
        return (-1, "".into(), fallback_label_color(), false);
    };
    let Some(label) = labels.iter().find(|label| label.id == label_id) else {
        return (
            label_id as i32,
            "Unknown label".into(),
            fallback_label_color(),
            false,
        );
    };
    (
        label_id as i32,
        label.name.clone().into(),
        settings::parse_color(&label.color, fallback_label_color()),
        true,
    )
}

pub fn tag_rows(tag_ids: &[u64], tags: &[TagDefinition]) -> ModelRc<TagRow> {
    ModelRc::new(VecModel::from(
        tag_ids
            .iter()
            .map(|tag_id| TagRow {
                id: *tag_id as i32,
                name: tags
                    .iter()
                    .find(|tag| tag.id == *tag_id)
                    .map(|tag| tag.name.clone())
                    .unwrap_or_else(|| "Unknown tag".to_owned())
                    .into(),
            })
            .collect::<Vec<_>>(),
    ))
}

pub fn available_tag_rows(tag_ids: &[u64], tags: &[TagDefinition]) -> ModelRc<TagRow> {
    ModelRc::new(VecModel::from(
        tags.iter()
            .filter(|tag| !tag_ids.contains(&tag.id))
            .map(|tag| TagRow {
                id: tag.id as i32,
                name: tag.name.clone().into(),
            })
            .collect::<Vec<_>>(),
    ))
}

pub fn update_comment_model(
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

pub fn update_audio_rows(
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
    let previous_path =
        ACTIVE_AUDIO_PATH.with(|current| current.replace(active_path.map(Path::to_path_buf)));
    let mut paths = Vec::new();
    if let Some(path) = previous_path.as_deref() {
        paths.push(path);
    }
    if let Some(path) = active_path {
        if previous_path.as_deref() != Some(path) {
            paths.push(path);
        }
    }
    for path in paths {
        let Some(index) = row_index(path) else {
            continue;
        };
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_active = active_path.is_some_and(|active| active == path);
        if row.is_active != is_active
            || row.is_playing != (is_active && is_playing)
            || (is_active && (row.progress - progress).abs() > 0.001)
        {
            model.set_row_data(
                index,
                AudioRow {
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
                    is_active,
                    is_playing: is_active && is_playing,
                    progress: if is_active { progress } else { row.progress },
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
        }
    }
}

pub fn update_audio_loading_rows(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    loading: &HashSet<PathBuf>,
    previous_loading: &mut HashSet<PathBuf>,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    let changed_paths = loading
        .symmetric_difference(previous_loading)
        .cloned()
        .collect::<Vec<_>>();
    previous_loading.clone_from(loading);
    for path in changed_paths {
        let Some(index) = row_index(&path) else {
            continue;
        };
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_loading = loading.contains(&path);
        if row.is_loading != is_loading {
            model.set_row_data(
                index,
                AudioRow {
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
                    is_loading,
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
                    tags: row.tags,
                    available_tags: row.available_tags,
                    detected_bpm: row.detected_bpm,
                },
            );
        }
    }
}

pub fn select_comment(
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
                    selected_comment_start: if selected { start } else { -1.0 },
                    selected_comment_end: if selected { end } else { -1.0 },
                    label_id: row.label_id,
                    label_name: row.label_name,
                    label_color: row.label_color,
                    label_known: row.label_known,
                    tags: row.tags,
                    available_tags: row.available_tags,
                    detected_bpm: row.detected_bpm,
                },
            );
        }
    }
}

pub fn select_audio_paths(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    selected_paths: &HashSet<PathBuf>,
    primary_path: &Path,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let row_path = Path::new(row.path.as_str());
        let is_selected = selected_paths.contains(row_path);
        let is_primary = row_path == primary_path;
        if row.is_selected != is_selected || row.is_primary != is_primary {
            model.set_row_data(
                index,
                AudioRow {
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
                    is_selected,
                    is_cut: row.is_cut,
                    is_primary,
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
        }
    }
}

pub fn select_audio_path(audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>, path: &Path) {
    let mut selected_paths = HashSet::new();
    selected_paths.insert(path.to_path_buf());
    select_audio_paths(audio_model, &selected_paths, path);
}

pub fn update_audio_cut_rows(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    cut_paths: &[PathBuf],
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_cut = cut_paths
            .iter()
            .any(|path| Path::new(row.path.as_str()) == path);
        if row.is_cut != is_cut {
            let mut updated = row.clone();
            updated.is_cut = is_cut;
            model.set_row_data(index, updated);
        }
    }
}

pub fn update_audio_subtitle(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    path: &Path,
    subtitle: &str,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        if Path::new(row.path.as_str()) == path && row.subtitle != subtitle {
            let mut updated = row.clone();
            updated.subtitle = subtitle.into();
            model.set_row_data(index, updated);
            break;
        }
    }
}

pub fn user_comment_subtitle(user_comments: &str) -> String {
    user_comments
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

pub fn scroll_to_path(
    window: &crate::MainWindow,
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
    let offset = (0..index)
        .filter_map(|row_index| model.row_data(row_index))
        .map(|row| {
            if row.is_folder {
                30.0
            } else if row.is_lora {
                96.0
            } else {
                132.0
            }
        })
        .sum::<f32>();
    window.set_audio_scroll_to_index(-1);
    window.set_audio_scroll_to_offset(offset.into());
}

pub fn scroll_to_path_if_needed(
    window: &crate::MainWindow,
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

    let viewport_start = window.get_audio_viewport_start().max(0) as usize;
    let visible_rows = window.get_audio_visible_rows().max(1) as usize;
    let viewport_end = viewport_start.saturating_add(visible_rows);
    let target_start = if index < viewport_start {
        Some(index)
    } else if index >= viewport_end {
        Some(index + 1 - visible_rows)
    } else {
        None
    };
    if let Some(target_start) = target_start {
        window.set_audio_scroll_to_index(target_start as i32);
    }
}

pub fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slint::Model;

    use super::{comment_rows_with_labels, format_duration};
    use crate::metadata::{AudioComment, AudioFileMetadata};

    #[test]
    fn comment_rows_sort_and_normalize_comment_ranges() {
        let metadata = AudioFileMetadata {
            duration_seconds: 10.0,
            comments: vec![
                AudioComment {
                    start_seconds: 8.0,
                    end_seconds: 12.0,
                    text: "late".into(),
                    label_id: None,
                },
                AudioComment {
                    start_seconds: -2.0,
                    end_seconds: 1.0,
                    text: "early".into(),
                    label_id: None,
                },
            ],
            ..Default::default()
        };

        let rows = comment_rows_with_labels(&metadata, &[]);

        assert_eq!(rows.row_count(), 2);
        assert_eq!(rows.row_data(0).unwrap().text, "early");
        assert_eq!(rows.row_data(0).unwrap().start, 0.0);
        assert_eq!(rows.row_data(0).unwrap().end, 0.1);
        assert_eq!(rows.row_data(0).unwrap().bubble_end, 0.8);
        assert_eq!(rows.row_data(1).unwrap().text, "late");
        assert_eq!(rows.row_data(1).unwrap().start, 0.8);
        assert_eq!(rows.row_data(1).unwrap().end, 1.0);
        assert_eq!(rows.row_data(1).unwrap().bubble_end, 1.0);
    }

    #[test]
    fn comment_rows_use_a_nonzero_duration_for_empty_metadata() {
        let metadata = AudioFileMetadata {
            comments: vec![AudioComment {
                start_seconds: 1.0,
                end_seconds: 2.0,
                text: "comment".into(),
                label_id: None,
            }],
            ..Default::default()
        };

        let row = comment_rows_with_labels(&metadata, &[])
            .row_data(0)
            .unwrap();

        assert_eq!(row.start, 1.0);
        assert_eq!(row.end, 1.0);
    }

    #[test]
    fn time_formatters_use_fixed_audio_display_formats() {
        assert_eq!(format_duration(Duration::from_secs(125)), "02:05");
    }
}
