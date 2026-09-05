use std::path::Path;

use slint::{ModelRc, VecModel};

use crate::{metadata, MainWindow, WorkflowLoraRow};

use super::file_system;

pub fn scan_json_files(folder: &Path) -> Vec<(String, String)> {
    fn visit(folder: &Path, files: &mut Vec<(String, String)>) {
        for entry in file_system::read_dir_sorted(folder) {
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
    window.set_workflow_key("".into());
    window.set_workflow_seed("".into());
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
    window.set_workflow_bpm(workflow.bpm.into());
    window.set_workflow_key(workflow.key.into());
    window.set_workflow_seed(workflow.seed.into());
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

pub fn load_workflow_for_audio(window: &MainWindow, folder: &Path, path: &Path) {
    let path_string = path.to_string_lossy();
    let workflow_path = metadata::load_index(folder)
        .audio_files
        .iter()
        .find(|file| file.file_path == path_string)
        .and_then(|file| file.workflow_json_path.clone());
    let Some(workflow_path) = workflow_path else {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        return;
    };
    match metadata::comfyui::parse_file(Path::new(&workflow_path)) {
        Ok(workflow) => apply_workflow(window, folder, &workflow_path, workflow),
        Err(error) => window.set_audio_error(format!("Workflow JSON: {error}").into()),
    }
}