use std::{
    cell::RefCell,
    path::Path,
    rc::Rc,
};

use slint::{ModelRc, VecModel};

use crate::{
    settings::{self, AppSettings},
    workspace::file_system::{self, TreeState},
    BreadcrumbRow, MainWindow, TreeRow,
};

pub fn select_tree_path(
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
    let filter = window.get_audio_filter().to_string();
    let tree_index = file_system::build_visible_rows(
        state,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    )
    .iter()
    .filter(|row| {
        file_system::is_visible_in_tree(row.kind)
            && (row.kind != file_system::FileKind::Audio
                || file_system::matches_audio_filter(&row.name, &filter))
    })
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

pub fn set_audio_breadcrumbs(window: &MainWindow, workspace: &Path, folder: &Path) {
    let mut rows = Vec::new();
    let mut path = workspace.to_path_buf();
    let workspace_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| workspace.to_str().unwrap_or("Workspace"));
    rows.push(BreadcrumbRow {
        name: workspace_name.to_owned().into(),
        path: path.to_string_lossy().into_owned().into(),
    });

    if let Ok(relative) = folder.strip_prefix(workspace) {
        for component in relative.components() {
            path.push(component.as_os_str());
            rows.push(BreadcrumbRow {
                name: component.as_os_str().to_string_lossy().into_owned().into(),
                path: path.to_string_lossy().into_owned().into(),
            });
        }
    }

    window.set_audio_breadcrumbs(ModelRc::new(VecModel::from(rows)));
}

pub fn refresh_tree(window: &MainWindow, tree_state: &Rc<RefCell<Option<TreeState>>>) {
    let state_ref = tree_state.borrow();
    let Some(state) = state_ref.as_ref() else {
        return;
    };
    window.set_tree_selection_count(state.selected_paths().len() as i32);

    let filter = window.get_audio_filter().to_string();
    let rows: Vec<TreeRow> = file_system::build_visible_rows(
        state,
        file_system::SortOrder::from_i32(window.get_sort_order()),
    )
    .into_iter()
    .filter(|row| {
        file_system::is_visible_in_tree(row.kind)
            && (row.kind != file_system::FileKind::Audio
                || file_system::matches_audio_filter(&row.name, &filter))
    })
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

