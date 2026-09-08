use std::path::Path;

use slint::{ModelRc, VecModel};

use crate::{metadata, MainWindow, WorkflowLoraRow};

use super::file_system;

fn key_index(key: &str) -> i32 {
    [
        "C major", "C minor", "C# major", "C# minor", "D major", "D minor",
        "Eb major", "Eb minor", "E major", "E minor", "F major", "F minor",
        "F# major", "F# minor", "G major", "G minor", "Ab major", "Ab minor",
        "A major", "A minor", "Bb major", "Bb minor", "B major", "B minor",
    ]
    .iter()
    .position(|candidate| candidate.eq_ignore_ascii_case(key))
    .map(|index| index as i32)
    .unwrap_or(-1)
}

fn display_model_name(model: &str) -> String {
    match model {
        "ace_step_1.5_turbo_aio.safetensors" => "Ace Step 1.5 Turbo Aio".to_string(),
        _ => model.to_string(),
    }
}

pub fn scan_json_files(folder: &Path) -> Vec<(String, String)> {
    fn visit(folder: &Path, files: &mut Vec<(String, String)>) {
        for entry in
            file_system::read_dir_sorted(folder, file_system::SortOrder::AlphabeticalAscending)
        {
            if entry.is_dir {
                if entry.name != ".adbstudio" {
                    visit(&entry.path, files);
                }
            } else if entry.kind == file_system::FileKind::Json {
                files.push((entry.name, entry.path.to_string_lossy().into_owned()));
            }
        }
    }
    let mut files = Vec::new();
    visit(folder, &mut files);
    files.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    files
}

pub fn clear_workflow(window: &MainWindow) {
    window.set_workflow_bpm("".into());
    window.set_workflow_bpm_number(0);
    window.set_workflow_key("".into());
    window.set_workflow_key_index(-1);
    window.set_workflow_seed("".into());
    window.set_workflow_seed_number(0);
    window.set_workflow_model("".into());
    window.set_workflow_prompt("".into());
    window.set_workflow_lyrics("".into());
    window.set_workflow_loras(ModelRc::new(VecModel::from(Vec::<WorkflowLoraRow>::new())));
}

pub fn apply_workflow(
    window: &MainWindow,
    folder: &Path,
    path: &str,
    workflow: metadata::comfyui::ComfyUIWorkflow,
) {
    let display_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    window.set_selected_workflow(display_name.into());
    let bpm_number = workflow.bpm.parse().unwrap_or(0);
    window.set_workflow_bpm(workflow.bpm.into());
    window.set_workflow_bpm_number(bpm_number);
    window.set_workflow_key_index(key_index(&workflow.key));
    window.set_workflow_key(workflow.key.into());
    let seed_number = workflow.seed.parse().unwrap_or(0);
    window.set_workflow_seed(workflow.seed.into());
    window.set_workflow_seed_number(seed_number);
    window.set_workflow_model(display_model_name(&workflow.model).into());
    window.set_workflow_prompt(workflow.prompt.into());
    window.set_workflow_lyrics(workflow.lyrics.into());
    let index = metadata::load_index(folder);
    window.set_workflow_loras(ModelRc::new(VecModel::from(
        workflow
            .loras
            .into_iter()
            .map(|lora| WorkflowLoraRow {
                custom_tag: index
                    .loras
                    .iter()
                    .find(|stored| stored.filename == lora.filename)
                    .map(|stored| stored.custom_tag.clone())
                    .unwrap_or_default()
                    .into(),
                filename: lora.filename.into(),
                strength: lora.strength.into(),
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn should_reload_workflow(current: Option<&Path>, next: &Path, force: bool) -> bool {
    if force {
        return true;
    }
    match current {
        Some(current) => current != next,
        None => true,
    }
}

pub fn load_workflow_for_audio(
    window: &MainWindow,
    folder: &Path,
    path: &Path,
    workflow_loading: &std::rc::Rc<std::cell::RefCell<bool>>,
    loaded_workflow_path: &std::rc::Rc<std::cell::RefCell<Option<std::path::PathBuf>>>,
    force: bool,
) {
    if window.get_workflow_loading() {
        return;
    }
    if !force && !should_reload_workflow(loaded_workflow_path.borrow().as_deref(), path, false) {
        return;
    }
    *workflow_loading.borrow_mut() = true;
    window.set_workflow_loading(true);
    window.set_workflow_modified(false);
    *loaded_workflow_path.borrow_mut() = Some(path.to_path_buf());
    let Some(workflow_path) = metadata::workflow_path(folder, path) else {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        window.set_workflow_modified(false);
        window.set_workflow_loading(false);
        *workflow_loading.borrow_mut() = false;
        return;
    };
    if !workflow_path.is_file() {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        window.set_workflow_modified(false);
        window.set_workflow_loading(false);
        *workflow_loading.borrow_mut() = false;
        return;
    }
    match metadata::comfyui::parse_file(&workflow_path) {
        Ok(workflow) => apply_workflow(window, folder, &workflow_path.to_string_lossy(), workflow),
        Err(error) => window.set_audio_error(format!("Workflow JSON: {error}").into()),
    }
    window.set_workflow_modified(false);
    window.set_workflow_loading(false);
    *workflow_loading.borrow_mut() = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_is_not_reloaded_without_force() {
        let current = std::path::Path::new("/workspace/audio.wav");
        let next = std::path::Path::new("/workspace/audio.wav");
        assert!(!should_reload_workflow(Some(current), next, false));
    }

    #[test]
    fn different_path_reloads_or_force_overrides() {
        let current = std::path::Path::new("/workspace/audio.wav");
        let next = std::path::Path::new("/workspace/other.wav");
        assert!(should_reload_workflow(Some(current), next, false));
        assert!(should_reload_workflow(Some(current), next, true));
    }
}
