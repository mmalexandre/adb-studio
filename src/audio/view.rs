use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use slint::{Model, ModelRc, VecModel};

use crate::metadata;
use crate::{AudioRow, CommentRow};

pub fn comment_rows(item: &metadata::AudioFileMetadata) -> ModelRc<CommentRow> {
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
                    is_loading: row.is_loading,
                    comments: row.comments,
                    differences: row.differences,
                    rating: row.rating,
                    is_pinned: row.is_pinned,
                    is_selected: row.is_selected,
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

pub fn update_audio_loading_rows(
    audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>,
    loading: &HashSet<PathBuf>,
) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_loading = loading.contains(Path::new(row.path.as_str()));
        if row.is_loading != is_loading {
            model.set_row_data(
                index,
                AudioRow {
                    path: row.path,
                    name: row.name,
                    modified_date: row.modified_date,
                    peaks: row.peaks,
                    is_loading,
                    comments: row.comments,
                    differences: row.differences,
                    rating: row.rating,
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
                    modified_date: row.modified_date,
                    peaks: row.peaks,
                    is_loading: row.is_loading,
                    comments: row.comments,
                    differences: row.differences,
                    rating: row.rating,
                    is_pinned: row.is_pinned,
                    is_selected: row.is_selected,
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

pub fn select_audio_path(audio_model: &Rc<RefCell<Option<Rc<VecModel<AudioRow>>>>>, path: &Path) {
    let Some(model) = audio_model.borrow().clone() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let is_selected = Path::new(row.path.as_str()) == path;
        if row.is_selected != is_selected {
            model.set_row_data(
                index,
                AudioRow {
                    path: row.path,
                    name: row.name,
                    modified_date: row.modified_date,
                    peaks: row.peaks,
                    is_loading: row.is_loading,
                    comments: row.comments,
                    differences: row.differences,
                    rating: row.rating,
                    is_pinned: row.is_pinned,
                    is_selected,
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
    window.set_audio_scroll_to_index(index as i32);
}

pub fn format_seconds(seconds: f32) -> String {
    format!("{seconds:.3}")
}

pub fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slint::Model;

    use super::{comment_rows, format_duration, format_seconds};
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
                },
                AudioComment {
                    start_seconds: -2.0,
                    end_seconds: 1.0,
                    text: "early".into(),
                },
            ],
            ..Default::default()
        };

        let rows = comment_rows(&metadata);

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
            }],
            ..Default::default()
        };

        let row = comment_rows(&metadata).row_data(0).unwrap();

        assert_eq!(row.start, 1.0);
        assert_eq!(row.end, 1.0);
    }

    #[test]
    fn time_formatters_use_fixed_audio_display_formats() {
        assert_eq!(format_seconds(1.2), "1.200");
        assert_eq!(format_duration(Duration::from_secs(125)), "02:05");
    }
}
