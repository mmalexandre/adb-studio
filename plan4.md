# Plan: Fix workflow-edit clobbering (seed loss) and spurious modified flag

## TL;DR
Bug 1 (edited Seed lost on "Open workflow file"): `load_workflow_for_audio` is called
unconditionally from Play, Seek, and LoRA-save handlers even when the audio path hasn't
changed. Each call re-parses the workflow from disk and forces `workflow_modified = false`,
so any pending edit is silently discarded before the user even clicks "Open workflow file" —
`on_open_workflow_requested` then takes the "not modified" branch and opens the ORIGINAL file.
Bug 2 (orange border with no edits): the Key `ComboBox` uses `changed current-index` (fires on
both user AND programmatic changes) unlike every other field's `edited(...)` (user-only), and
its guard (`workflow_loading`) can already be cleared by the time a deferred internal Slint
event dispatches, causing `workflow_modified` to spuriously flip true right after a track loads.

Fix: track "which audio path is currently loaded" and skip full reloads when unchanged
(preserving in-flight edits) for Play/Seek/LoRA-save; tag `edited_workflow` with its owning
path so stale cross-track reuse is impossible; harden the Key ComboBox path against
deferred-callback reentrancy by deferring the `workflow_loading` guard release by one event-loop
tick.

**Decisions (confirmed with user)**
- Play/Pause/Resume/Seek: skip reload whenever path == already-loaded path, regardless of
  `workflow_modified` (no re-read from disk on unchanged path).
- LoRA save: keep a full reload, but route it through the same "preserve edits" guard/helper
  instead of building a separate lightweight LoRA-only refresh.

**Steps**

### Phase 1 — Add a single source of truth for "currently loaded workflow path" (foundation)
1. Add a new field to `AppState` (src/app/mod.rs): `pub loaded_workflow_path: Rc<RefCell<Option<PathBuf>>>`,
   initialized to `None`. Clone it alongside `workflow_loading` wherever that's threaded through
   (main.rs wiring, tree_controller, playback_controller, workflow_controller register fns).
2. In `load_workflow_for_audio` (src/workspace/workflow.rs), add a `force: bool` parameter (or a
   sibling wrapper fn). At the top, if `!force` and `loaded_workflow_path.borrow().as_deref() == Some(path)`,
   return immediately without touching `workflow_modified` or re-parsing from disk.
   On every successful/attempted load, set `*loaded_workflow_path.borrow_mut() = Some(path.to_path_buf())`
   (or `None` on the "no workflow file" early-return branches, matching existing clear_workflow behavior).
3. Update call sites:
   - `on_audio_row_selected` (src/app/tree_controller.rs#L48): call with `force: true` (real track
     switch — must always clear edits and reload). This is *depends on step 1-2*.
   - `on_audio_play` (src/app/playback_controller.rs#L224) and `on_audio_seek` (#L282): call with
     `force: false` so same-path replays/scrubs no-op. *depends on step 1-2*.
   - `on_lora_save` (src/app/workflow_controller.rs#L330): call with `force: false` too — per
     decision, reuse the same guard/helper rather than a bespoke LoRA-only refresh. *depends on step 1-2*.
4. Verify `clear_workflow` call sites (e.g. `set_workspace` in src/workspace/lifecycle.rs) also
   reset `loaded_workflow_path` to `None` so a freshly opened workspace doesn't wrongly suppress
   the very first load.

### Phase 2 — Tag `edited_workflow` with its owning path (closes the "wrong track" hole)
5. Change `edited_workflow`'s shape to also track the path it was captured for — simplest: add a
   sibling `Rc<RefCell<Option<PathBuf>>> edited_workflow_path` next to the existing
   `Rc<RefCell<Option<serde_json::Value>>>` (avoids touching every call site's tuple destructuring).
   *depends on Phase 1 completing (shares wiring pattern)*.
6. Update `ensure_edit_copy` (src/app/workflow_controller.rs#L403) to check that
   `edited_workflow_path.borrow().as_deref() == Some(current_selected_path)` in addition to
   `.is_some()`; if mismatched, treat as stale — reload fresh from disk for the new path and
   update `edited_workflow_path` accordingly.
7. Set `edited_workflow_path` alongside every existing `*edited_workflow.borrow_mut() = ...` write
   (tree_controller.rs#L43 reset to None, workflow_controller.rs ensure_edit_copy population,
   recreate_workflow success reset to None).

### Phase 3 — Harden Key ComboBox against deferred-callback reentrancy (Bug 2)
8. In src/workspace/workflow.rs `load_workflow_for_audio`, defer clearing the loading guard by one
   event-loop tick instead of clearing it synchronously right after `apply_workflow` returns: wrap
   the final `window.set_workflow_modified(false); window.set_workflow_loading(false); *workflow_loading.borrow_mut() = false;`
   in `slint::Timer::single_shot(std::time::Duration::ZERO, move || { ... })` (captures weak window
   + Rc clones). This ensures any reentrant `changed current-index` queued during `apply_workflow`
   still observes `workflow_loading == true` when it actually dispatches. *independent of Phase 1/2,
   can run in parallel*.
9. Defense in depth: in ui/main.slint, change the Key `ComboBox`'s `changed current-index` handler
   to mirror the guard pattern already implicit in other fields — add
   `if !root.workflow-loading { root.workflow-metadata-changed("key", self.model[self.current-index]); }`
   (around line 1518) so the emission itself is guarded at the point of firing, not just in Rust.

### Phase 4 — Verification
10. Manual repro test (previously failing): select a track with an existing workflow, edit Seed,
    click Play (or scrub waveform) on that same track, then click "Open workflow file" — confirm
    the opened file now contains the edited seed.
11. Manual repro test for Bug 2: rapidly switch between several tracks with different/missing Key
    values and confirm the metadata pane border never turns orange without an explicit edit.
12. `cargo check` and `cargo test` after each phase; existing tests in
    src/metadata/comfyui/updater.rs and parser.rs should remain green (no changes needed there —
    the seed-persistence bug was never in the JSON-patching logic).
13. Add a small unit test if a pure helper is extracted (e.g. a `should_reload(current, new, force) -> bool`
    function) to lock in the "skip reload when path unchanged and not forced" behavior without
    needing a Slint test harness.

**Relevant files**
- `src/workspace/workflow.rs` — `load_workflow_for_audio` (guard + deferred loading-flag clear), `clear_workflow`
- `src/app/mod.rs` — `AppState` new fields (`loaded_workflow_path`, `edited_workflow_path`)
- `src/app/tree_controller.rs` — `on_audio_row_selected` (force reload + reset both caches)
- `src/app/playback_controller.rs` — `on_audio_play` (#L224), `on_audio_seek` (#L282) (non-forced reload)
- `src/app/workflow_controller.rs` — `on_lora_save` (#L330, non-forced reload), `ensure_edit_copy` (#L403, path check), `on_workflow_metadata_changed`/`on_workflow_number_changed` (no change needed beyond existing seed string sync already applied)
- `src/workspace/lifecycle.rs` — `set_workspace` (reset `loaded_workflow_path` on workspace switch)
- `ui/main.slint` — Key `ComboBox` `changed current-index` guard (~line 1518)

**Scope boundaries**
- NOT touching `metadata::comfyui::update_metadata` / parser logic — the seed JSON-patching itself
  is already correct and tested; this is purely an application-state/lifecycle bug.
- NOT resetting `edited_workflow` after a successful "Open workflow file" or "Run workflow" — out
  of scope unless user reports it as a separate issue; current behavior (keep editing, re-open
  reflects further edits) is preserved.
- Previous point-fix (`window.set_workflow_seed(...)` sync in `on_workflow_number_changed`) stays;
  it's harmless and still needed for the string/number property consistency, just wasn't sufficient
  alone.
