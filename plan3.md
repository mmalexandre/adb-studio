# Plan: Modularize ADB Studio (AppState + controllers)

## Confirmed decisions
- Architecture: introduce `AppState` struct holding all shared Rc<RefCell<>>/Arc<Mutex<>> fields.
  Each subsystem becomes a controller module exposing `install(state: &Rc<AppState>, window: &MainWindow)`
  to register slint callbacks, and (where relevant) a `tick(state: &Rc<AppState>, window: &MainWindow)`
  called from one central timer in app/mod.rs.
- Also split sync/mod.rs (1128 lines) and metadata/comfyui.rs (847 lines).
- Do the work in independently buildable/testable phases (cargo build + cargo test after each).
- Small, obviously-safe bug fixes allowed along the way if noticed; anything that changes UX/feature
  behavior must be called out explicitly, not silently bundled.
- Target: every file 300-500 lines max.

## Verified facts (see /memories/repo/adbstudio-main-analysis.md)
- main.rs is actually ~3650 lines (grep_search undercounted it as 1001 - bug, verified w/ read_file).
- sync/mod.rs ~1128 lines, metadata/comfyui.rs 847 lines also exceed budget.
- All other src files already under ~310 lines.
- src/app/ dir exists but is empty - designated home for new controller modules.

## main.rs full inventory (from direct read, all ~50 window.on_* handlers + helpers)
State vars in fn main(): settings, tree_state, audio_folder, workflow_files, edited_workflow,
workflow_loading, recreate_workflow_pending, workspace_watcher, workspace_change_sender/receiver,
audio_model, sync_controller, comment_editor_original, comment_editor_duration, last_button_click,
conversion_receiver/cancelled/jobs/temp_root/target/model, workflow_run_sender/receiver/cancelled,
playback, audio_result_sender/receiver, audio_load_state.

Handler groups identified:
- Window/app chrome: on_toggle_fullscreen, on_theme_selected, on_close_about/settings/tips,
  on_tips_preference_changed, on_left_pane_width_changed, on_metadata_pane_height_changed,
  on_metadata_toggle, splash timer, window.show()/run(), settings::restore_window/save_window.
- Keyboard shortcuts: on_shortcut_changed, on_global_key_pressed (+ seek math), on_seek_seconds_changed.
- Playback: on_volume_changed, on_audio_play_pause, on_audio_play, on_audio_seek, on_audio_navigate,
  on_row_clicked (play part), on_comment_selected (play part), comment_duration(), save_playback_position().
- Audio list/loading: on_audio_viewport_changed, on_filter_changed, on_sort_order_selected (list part),
  refresh_audio/refresh_audio_for_changes/refresh_audio_with_changes, matches_audio_filter,
  request_audio_generation, set_audio_breadcrumbs, timer polling audio_result_receiver + loading rows.
- Tree/file navigation: on_chevron_clicked, on_breadcrumb_requested, on_tree_reveal_requested,
  on_rename_requested, on_new_folder_requested, on_tree_edit_accepted/cancelled, on_tree_drop_requested,
  on_row_clicked (tree part), on_audio_row_selected, select_tree_path, refresh_tree.
- Files manager (actual fs mutations): move-to-trash closure, on_trash_requested/confirmed/cancelled,
  on_pin_requested, rename_associated_workflow calls, file rename/move/create-dir operations.
- Metadata pane / comments: on_comment_range_requested/moved/edit_requested/save/delete/cancel,
  on_rating_requested, on_comment_colors_selected.
- Workflow editing/run: on_workflow_metadata_changed/number_changed, on_lora_edit_requested/save/cancel,
  on_recreate_workflow_requested, on_open_workflow_requested, on_run_workflow_requested/cancelled/closed,
  recreate_workflow(), ensure_edit_copy(), write_modified_workflow(), open_comfyui_workflow(),
  timer polling workflow_run_receiver.
- Conversion: on_conversion_requested/started/cancelled/closed, timer polling conversion_receiver +
  finalize-on-complete logic (trash source, rename temp->dest, rename workflow sidecar).
- Sync/ComfyUI config: on_comfyui_sync_requested/choose_directory/test/save/cancel,
  timer polling sync_controller.events(), recreate_workflow_pending flow.
- Workspace lifecycle: on_open_folder, on_close_folder, set_workspace(), close_workspace(),
  refresh_workspace(), is_internal_path(), workspace watcher (notify) setup + debounce in timer.
- fn main() itself: window creation, settings load, initial state construction, initial window.set_*
  calls from settings, wiring all controllers, final window.show()/run()/shutdown save.

## Steps

### Phase 0 - scaffolding (no behavior change)
1. Create `src/app/mod.rs` with `AppState` struct (all fields from fn main(), same Rc/RefCell/Arc
   types) + `AppState::new(...)` constructor, and `pub fn run() -> Result<...>` that will eventually
   replace fn main()'s body. main.rs's `fn main()` becomes a 2-3 line shim calling `app::run()`.
2. Move `sync_cursor_environment`, `gsettings_value` (env/cursor setup, unrelated to app state) into
   `src/app/env_setup.rs` (~40 lines) since they're standalone.
3. Verify: `cargo build` succeeds with app::run() just containing the old fn main() body inlined
   (mechanical move only, not yet split into controllers). *depends on nothing, blocks all later steps*

### Phase 1 - split sync/mod.rs and metadata/comfyui.rs (independent of main.rs work, *parallel with Phase 2*)
4. Split `src/sync/mod.rs` into:
   - `src/sync/mod.rs` (~250 lines): SyncConfig, RemoteFile, DownloadRecord/Index, SyncProgress,
     SyncEvent, SyncController, config_path/load_config/save_config/ensure_destination, re-exports.
   - `src/sync/client.rs` (~350 lines): ComfyUiClient impl (list_files, upload_workflow, run_workflow,
     interrupt, progress, download_view, download_file, download_optional_file), AudioOutput,
     find_audio_output, output_file_extension, response_error, validate_request_config.
   - `src/sync/worker.rs` (~250 lines): sync_loop, sync_workflow, workflow_local_path,
     load_download_index/save_download_index, progress_with_index, sort_remote_files,
     temporary_download_path, SyncError.
   - `src/sync/tests.rs` or keep `#[cfg(test)] mod tests` split per file next to what they test.
5. Split `src/metadata/comfyui.rs` into:
   - `src/metadata/comfyui/mod.rs` (~120 lines): ComfyUIWorkflow, LoRAInfo, TrackDifference structs,
     `pub use` parser::*, diff::*.
   - `src/metadata/comfyui/parser.rs` (~350 lines): parse_file, parse_value, update_metadata, visit,
     parse_visual_node, find_string, find_scalar, scalar_text.
   - `src/metadata/comfyui/diff.rs` (~200 lines): compare_files, add_*_difference fns, format_number,
     edit_distance.
   - Move `#[cfg(test)] mod tests` blocks to sit next to the code they cover (parser tests in
     parser.rs, diff/edit_distance tests in diff.rs).
6. Verify: `cargo build && cargo test` - all 8 existing comfyui tests + sync tests still pass, same
   public API (`metadata::comfyui::{...}`, `sync::{...}`) so call sites elsewhere need zero changes.

### Phase 2 - extract AppState fields and pure helper functions (*depends on Phase 0*)
7. Move these standalone functions (no window.on_* wiring, already close to pure) out of main.rs:
   - `src/workspace/tree_nav.rs`: select_tree_path, refresh_tree (~120 lines)
   - `src/workspace/library.rs`: refresh_audio/refresh_audio_for_changes/refresh_audio_with_changes,
     matches_audio_filter, track_differences, set_audio_breadcrumbs, request_audio_generation
     (~350 lines) - this is the "audio list assembled from filesystem + metadata index" logic.
   - `src/workspace/lifecycle.rs`: set_workspace, close_workspace, refresh_workspace,
     is_internal_path (~250 lines).
   - `src/audio/session.rs`: comment_duration, save_playback_position (~60 lines).
8. Verify: `cargo build` (these are free functions taking explicit params, same signatures, just
   relocated + `pub(crate)` - mechanical move, update `use` paths in main.rs).

### Phase 3 - controller modules (*depends on Phase 2*, each controller can be done in parallel
  once AppState exists, but merge sequentially to avoid churn)
9. `src/app/window_controller.rs` (~200 lines): install() wires on_toggle_fullscreen, on_theme_selected,
   on_close_about/settings/tips, on_tips_preference_changed, on_left_pane_width_changed,
   on_metadata_pane_height_changed, on_metadata_toggle; splash timer setup.
10. `src/app/keyboard_controller.rs` (~200 lines): install() wires on_shortcut_changed,
    on_global_key_pressed, on_seek_seconds_changed.
11. `src/app/playback_controller.rs` (~450 lines): install() wires on_volume_changed,
    on_audio_play_pause, on_audio_play, on_audio_seek, on_audio_navigate, playback part of
    on_row_clicked and on_comment_selected; tick() handles playback position/loop/auto-advance
    logic currently in the mega timer.
12. `src/app/audio_loading_controller.rs` (~350 lines): install() wires on_audio_viewport_changed,
    on_filter_changed, audio part of on_sort_order_selected; tick() handles audio_result_receiver
    draining + loading-row updates (loader/waveform polling).
13. `src/app/tree_controller.rs` (~400 lines): install() wires on_chevron_clicked,
    on_breadcrumb_requested, on_tree_reveal_requested, on_rename_requested, on_new_folder_requested,
    on_tree_edit_accepted/cancelled, on_tree_drop_requested, tree part of on_row_clicked,
    on_audio_row_selected.
14. `src/app/files_manager.rs` (~250 lines): move_to_trash logic, on_trash_requested/confirmed/cancelled,
    on_pin_requested - the actual filesystem mutation + workflow-sidecar-rename calls, called by
    tree_controller where needed.
15. `src/app/metadata_pane_controller.rs` (~450 lines): install() wires on_comment_range_requested/
    moved/edit_requested/save/delete/cancel, on_rating_requested, on_comment_colors_selected.
16. `src/app/workflow_controller.rs` (~450 lines): install() wires on_workflow_metadata_changed/
    number_changed, on_lora_edit_requested/save/cancel, on_recreate_workflow_requested,
    on_open_workflow_requested, on_run_workflow_requested/cancelled/closed, recreate_workflow(),
    ensure_edit_copy(), write_modified_workflow(), open_comfyui_workflow(); tick() drains
    workflow_run_receiver.
17. `src/app/conversion_controller.rs` (~300 lines): install() wires on_conversion_requested/started/
    cancelled/closed; tick() drains conversion_receiver + finalizes completed jobs.
18. `src/app/sync_ui_controller.rs` (~300 lines): install() wires on_comfyui_sync_requested/
    choose_directory/test/save/cancel; tick() drains sync_controller.events().
19. `src/app/workspace_controller.rs` (~250 lines): install() wires on_open_folder, on_close_folder;
    owns workspace watcher setup + workspace_change debounce, calling workspace::lifecycle fns.
20. Verify after each controller lands: `cargo build` (warnings for now-unused old code removed as we go).

### Phase 4 - central wiring + single timer (*depends on Phase 3 complete*)
21. `src/app/mod.rs::run()`: build MainWindow, build AppState, call every controller's `install()`,
    start one `slint::Timer` whose closure calls each controller's `tick()` in the same order the
    old mega-closure did (workflow_run -> conversion -> workspace_change -> playback -> audio_load
    -> sync), open last folder, `window.show()?`, splash timer, `window.run()?`, shutdown save.
22. Slim `src/main.rs` down to just `mod app; fn main() -> Result<...> { app::run() }` plus
    `slint::include_modules!()` and the mod declarations (~30-50 lines total).
23. Verify: `cargo build && cargo test` - full test suite green, manual smoke test (see below).

## Relevant files
- `src/main.rs` - split per above; ends up as thin entry point only.
- `src/sync/mod.rs` - split into mod.rs/client.rs/worker.rs.
- `src/metadata/comfyui.rs` - split into comfyui/mod.rs/parser.rs/diff.rs.
- `src/app/` (currently empty) - new home for all controller modules + AppState.
- `src/workspace/` - gains tree_nav.rs, library.rs, lifecycle.rs.
- `src/audio/` - gains session.rs.
- Existing `src/audio/{playback,loader,waveform,view,conversion}.rs`, `src/settings/*`,
  `src/workspace/{file_system,pinned_track_sort,preferences,workflow}.rs`, `src/metadata/mod.rs`
  stay as-is (already reused, already within budget).

## Verification
1. `cargo build` after every phase/sub-step (catch borrow-checker/ownership breakage early since
   Rc/RefCell clones move between files).
2. `cargo test` after Phase 1 and Phase 4 minimum (existing 11+ tests: metadata, comfyui parsing,
   sync config/download-index, main.rs's audio filter test moves to workspace/library.rs).
3. Manual smoke test after Phase 4: open a workspace, play/pause/seek a track, add/edit/delete a
   comment, rate a track, pin a track, rename/trash a file, drag-drop a file, convert a file,
   configure + run ComfyUI sync, run a workflow, resize panes, toggle theme/fullscreen, keyboard
   shortcuts (play/pause, seek, navigate, trash, cancel edit, fullscreen, metadata toggle).
4. Confirm every new/split file is between ~150-500 lines (spot check with read_file bisection,
   NOT grep_search line counts, since grep_search has demonstrated undercounting on large files).

## Decisions
- AppState + controller (install/tick) pattern chosen over mechanical-only split.
- sync/mod.rs and metadata/comfyui.rs included in scope.
- Phased, independently buildable/testable approach.
- Pure refactor is the default goal, but small obviously-safe bug fixes noticed along the way are
  allowed; anything that changes UX/feature behavior gets called out explicitly rather than bundled
  silently.
- Slint UI files (ui/*.slint) are out of scope - no callback signatures change, only Rust-side wiring.
