use std::{fs, path::Path};

use slint::{ModelRc, VecModel};

use crate::{metadata, MainWindow, WorkflowLoraRow};

use super::file_system;

fn key_index(key: &str) -> i32 {
    [
        "C major", "C minor", "C# major", "C# minor", "D major", "D minor", "Eb major", "Eb minor",
        "E major", "E minor", "F major", "F minor", "F# major", "F# minor", "G major", "G minor",
        "Ab major", "Ab minor", "A major", "A minor", "Bb major", "Bb minor", "B major", "B minor",
    ]
    .iter()
    .position(|candidate| candidate.eq_ignore_ascii_case(key))
    .map(|index| index as i32)
    .unwrap_or(-1)
}

fn model_index(model: &str) -> i32 {
    [
        "ace_step_1.5_turbo_aio.safetensors",
        "ace_step_1.5_sft.safetensors",
        "ace_step_1.5_base.safetensors",
    ]
    .iter()
    .position(|candidate| candidate.eq_ignore_ascii_case(model))
    .map(|index| index as i32)
    .unwrap_or(-1)
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
    window.set_workflow_duration_minutes(0);
    window.set_workflow_duration_seconds(0);
    window.set_workflow_key("".into());
    window.set_workflow_key_index(-1);
    window.set_workflow_seed("".into());
    window.set_workflow_seed_number(0);
    window.set_workflow_ksampler_cfg("".into());
    window.set_workflow_ksampler_steps("".into());
    window.set_workflow_ksampler_steps_number(0);
    window.set_workflow_reference_audio("".into());
    window.set_workflow_reference_audio_hash("".into());
    window.set_workflow_reference_audio_resolved(false);
    window.set_workflow_reference_audio_ambiguous(false);
    window.set_workflow_reference_audio_guess_attempted(false);
    window.set_workflow_model("".into());
    window.set_workflow_model_index(-1);
    window.set_workflow_prompt("".into());
    window.set_workflow_lyrics("".into());
    window.set_workflow_loras(ModelRc::new(VecModel::from(Vec::<WorkflowLoraRow>::new())));
    window.set_workflow_recreated(false);
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
    let duration_seconds = workflow
        .duration
        .parse::<f64>()
        .unwrap_or(0.0)
        .max(0.0)
        .round() as i32;
    window.set_workflow_duration_minutes(duration_seconds / 60);
    window.set_workflow_duration_seconds(duration_seconds % 60);
    window.set_workflow_key_index(key_index(&workflow.key));
    window.set_workflow_key(workflow.key.into());
    let seed_number = workflow.seed.parse().unwrap_or(0);
    window.set_workflow_seed(workflow.seed.into());
    window.set_workflow_seed_number(seed_number);
    window.set_workflow_ksampler_cfg(workflow.ksampler_cfg.into());
    window.set_workflow_ksampler_steps_number(workflow.ksampler_steps.parse().unwrap_or(0));
    window.set_workflow_ksampler_steps(workflow.ksampler_steps.into());
    window.set_workflow_reference_audio(workflow.reference_audio.into());
    window.set_workflow_reference_audio_hash(workflow.reference_audio_hash.clone().into());
    window.set_workflow_reference_audio_resolved(
        !workflow.reference_audio_hash.is_empty() && !workflow.reference_audio_ambiguous,
    );
    window.set_workflow_reference_audio_ambiguous(workflow.reference_audio_ambiguous);
    window.set_workflow_reference_audio_guess_attempted(workflow.reference_audio_guess_attempted);
    window.set_workflow_model_index(model_index(&workflow.model));
    window.set_workflow_model(workflow.model.into());
    window.set_workflow_prompt(workflow.prompt.into());
    window.set_workflow_lyrics(workflow.lyrics.into());
    window.set_workflow_loras(ModelRc::new(VecModel::from(
        workflow
            .loras
            .into_iter()
            .map(|lora| WorkflowLoraRow {
                node_id: lora.node_id.into(),
                source_filename: lora.filename.clone().into(),
                custom_path: lora.custom_path.into(),
                custom_tag: metadata::load_lora_custom_tag(folder, &lora.filename).into(),
                filename: metadata::comfyui::display_lora_name(&lora.filename).into(),
                strength: lora.strength.into(),
            })
            .collect::<Vec<_>>(),
    )));
}

fn is_reference_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("flac" | "mp3" | "ogg" | "opus" | "wav" | "m4a" | "aiff" | "aif")
    )
}

pub fn find_reference_audio_matches(folder: &Path, filename: &str) -> Vec<std::path::PathBuf> {
    let relative_path = Path::new(filename);
    if relative_path.components().count() > 1 {
        let path = folder.join(relative_path);
        if path.is_file() && is_reference_audio(&path) {
            return vec![path];
        }
        return Vec::new();
    }

    fn visit(folder: &Path, filename: &str, matches: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(folder) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(".adbstudio") {
                continue;
            }
            if path.is_dir() {
                visit(&path, filename, matches);
            } else if is_reference_audio(&path)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case(filename))
            {
                matches.push(path);
            }
        }
    }

    let mut matches = Vec::new();
    visit(folder, filename, &mut matches);
    matches.sort();
    matches
}

fn find_reference_audio_files(folder: &Path) -> Vec<std::path::PathBuf> {
    fn visit(folder: &Path, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(folder) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(".adbstudio") {
                continue;
            }
            if path.is_dir() {
                visit(&path, files);
            } else if is_reference_audio(&path) {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(folder, &mut files);
    files.sort();
    files
}

pub fn find_reference_audio_by_hash(
    folder: &Path,
    _filename: &str,
    expected_hash: &str,
) -> Result<std::path::PathBuf, String> {
    if expected_hash.is_empty() {
        return Err("Reference audio has no checksum".into());
    }
    find_reference_audio_files(folder)
        .into_iter()
        .find(|path| metadata::hash_file(path).is_ok_and(|hash| hash == expected_hash))
        .ok_or_else(|| "Could not find reference audio with the stored checksum".into())
}

fn resolve_reference_audio(
    folder: &Path,
    workflow: &mut serde_json::Value,
    parsed: &mut metadata::comfyui::ComfyUIWorkflow,
) -> bool {
    if parsed.reference_audio.is_empty() || parsed.reference_audio_guess_attempted {
        return false;
    }
    let matches = find_reference_audio_matches(folder, &parsed.reference_audio);
    let (hash, ambiguous) = match matches.as_slice() {
        [path] => (metadata::hash_file(path).unwrap_or_default(), false),
        [] => (String::new(), false),
        _ => (String::new(), true),
    };
    parsed.reference_audio_hash = hash.clone();
    parsed.reference_audio_guess_attempted = true;
    parsed.reference_audio_ambiguous = ambiguous;
    metadata::comfyui::set_reference_audio_metadata(workflow, &hash, true, ambiguous);
    true
}

pub fn refresh_workflow_loras(window: &MainWindow, folder: &Path, value: &serde_json::Value) {
    window.set_workflow_loras(ModelRc::new(VecModel::from(
        metadata::comfyui::parse_value(value)
            .loras
            .into_iter()
            .map(|lora| WorkflowLoraRow {
                node_id: lora.node_id.into(),
                source_filename: lora.filename.clone().into(),
                custom_path: lora.custom_path.into(),
                custom_tag: metadata::load_lora_custom_tag(folder, &lora.filename).into(),
                filename: metadata::comfyui::display_lora_name(&lora.filename).into(),
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
    window.set_user_comments(
        metadata::load_audio_metadata(folder, path)
            .user_comments
            .into(),
    );
    let Some(workflow_path) = metadata::workflow_path(folder, path) else {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        window.set_workflow_runnable(false);
        window.set_workflow_modified(false);
        window.set_workflow_loading(false);
        *workflow_loading.borrow_mut() = false;
        return;
    };
    if !workflow_path.is_file() {
        window.set_selected_workflow("".into());
        clear_workflow(window);
        window.set_workflow_runnable(false);
        window.set_workflow_modified(false);
        window.set_workflow_recreated(false);
        window.set_workflow_loading(false);
        *workflow_loading.borrow_mut() = false;
        return;
    }
    let runnable = fs::read_to_string(&workflow_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .is_some_and(|value| metadata::comfyui::is_runnable_audio_workflow(&value));
    window.set_workflow_runnable(runnable);
    match fs::read_to_string(&workflow_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
    {
        Some(mut value) => {
            metadata::apply_lora_custom_paths(folder, &mut value);
            let mut parsed = metadata::comfyui::parse_value(&value);
            if resolve_reference_audio(folder, &mut value, &mut parsed) {
                if let Ok(contents) = serde_json::to_string_pretty(&value) {
                    let _ = fs::write(&workflow_path, contents);
                }
            }
            apply_workflow(window, folder, &workflow_path.to_string_lossy(), parsed);
            metadata::clear_workflow_recreated(folder, path);
        }
        None => window.set_audio_error("Workflow JSON is invalid".into()),
    }
    window.set_workflow_recreated(false);
    window.set_workflow_modified(false);
    window.set_workflow_loading(false);
    *workflow_loading.borrow_mut() = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("adb-studio-workflow-{suffix}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    #[test]
    fn key_lookup_is_case_insensitive_and_unknown_keys_are_missing() {
        assert_eq!(key_index("c major"), 0);
        assert_eq!(key_index("B MINOR"), 23);
        assert_eq!(key_index("H major"), -1);
    }

    #[test]
    fn model_index_matches_supported_filenames() {
        assert_eq!(model_index("ace_step_1.5_turbo_aio.safetensors"), 0);
        assert_eq!(model_index("ace_step_1.5_sft.safetensors"), 1);
        assert_eq!(model_index("ace_step_1.5_base.safetensors"), 2);
        assert_eq!(model_index("other.safetensors"), -1);
    }

    #[test]
    fn scan_json_files_is_recursive_and_skips_internal_metadata() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("nested/.adbstudio")).unwrap();
        fs::write(temp.0.join("z.json"), "{}").unwrap();
        fs::write(temp.0.join("nested/a.JSON"), "{}").unwrap();
        fs::write(temp.0.join("nested/.adbstudio/hidden.json"), "{}").unwrap();
        fs::write(temp.0.join("song.wav"), []).unwrap();

        let files = scan_json_files(&temp.0);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "a.JSON");
        assert_eq!(files[1].0, "z.json");
    }

    #[test]
    fn reference_audio_matching_is_case_insensitive_and_audio_only() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("nested/reference.WAV"), b"audio").unwrap();
        fs::write(temp.0.join("reference.txt"), b"not audio").unwrap();

        let matches = find_reference_audio_matches(&temp.0, "reference.wav");

        assert_eq!(matches, vec![temp.0.join("nested/reference.WAV")]);
    }

    #[test]
    fn reference_audio_resolution_hashes_one_match_and_marks_attempted() {
        let temp = TempDirectory::new();
        fs::write(temp.0.join("reference.wav"), b"audio").unwrap();
        let mut value = serde_json::json!({});
        let mut parsed = metadata::comfyui::ComfyUIWorkflow {
            reference_audio: "reference.wav".into(),
            ..Default::default()
        };

        assert!(resolve_reference_audio(&temp.0, &mut value, &mut parsed));
        assert!(parsed.reference_audio_guess_attempted);
        assert!(!parsed.reference_audio_hash.is_empty());
        assert_eq!(
            value["_adb_studio"]["reference_audio_guess_attempted"],
            true
        );
        assert!(!resolve_reference_audio(&temp.0, &mut value, &mut parsed));
    }

    #[test]
    fn reference_audio_resolution_leaves_ambiguous_matches_unhashed() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("reference.wav"), b"one").unwrap();
        fs::write(temp.0.join("nested/reference.wav"), b"two").unwrap();
        let mut value = serde_json::json!({});
        let mut parsed = metadata::comfyui::ComfyUIWorkflow {
            reference_audio: "reference.wav".into(),
            ..Default::default()
        };

        resolve_reference_audio(&temp.0, &mut value, &mut parsed);

        assert!(parsed.reference_audio_guess_attempted);
        assert!(parsed.reference_audio_ambiguous);
        assert!(parsed.reference_audio_hash.is_empty());
        assert_eq!(value["_adb_studio"]["reference_audio_ambiguous"], true);
    }

    #[test]
    fn reference_audio_matching_uses_relative_path_when_provided() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("reference.wav"), b"same audio").unwrap();
        fs::write(temp.0.join("nested/reference.wav"), b"same audio").unwrap();

        let matches = find_reference_audio_matches(&temp.0, "nested/reference.wav");

        assert_eq!(matches, vec![temp.0.join("nested/reference.wav")]);
    }

    #[test]
    fn reference_audio_hash_matching_ignores_filename() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("first.wav"), b"same audio").unwrap();
        fs::write(temp.0.join("nested/different-name.wav"), b"same audio").unwrap();
        let expected_hash = metadata::hash_file(&temp.0.join("first.wav")).unwrap();

        let path =
            find_reference_audio_by_hash(&temp.0, "missing-name.wav", &expected_hash).unwrap();

        assert!(
            path == temp.0.join("first.wav") || path == temp.0.join("nested/different-name.wav")
        );
    }
}
