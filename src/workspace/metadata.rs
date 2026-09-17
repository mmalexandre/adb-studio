use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::settings::{AppSettings, LabelDefinition, TagDefinition};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceMetadata {
    #[serde(default = "default_label_definitions")]
    pub label_definitions: Vec<LabelDefinition>,
    #[serde(default = "default_tag_definitions")]
    pub tag_definitions: Vec<TagDefinition>,
    #[serde(default = "default_next_label_id")]
    pub next_label_id: u64,
    #[serde(default = "default_next_tag_id")]
    pub next_tag_id: u64,
}

impl Default for WorkspaceMetadata {
    fn default() -> Self {
        let settings = AppSettings::default();
        Self {
            label_definitions: settings.label_definitions,
            tag_definitions: settings.tag_definitions,
            next_label_id: settings.next_label_id,
            next_tag_id: settings.next_tag_id,
        }
    }
}

impl WorkspaceMetadata {
    pub fn apply_to_settings(self, settings: &mut AppSettings) {
        settings.label_definitions = self.label_definitions;
        settings.tag_definitions = self.tag_definitions;
        settings.next_label_id = self.next_label_id;
        settings.next_tag_id = self.next_tag_id;
        settings.normalize_next_ids();
    }

    fn from_settings(settings: &AppSettings) -> Self {
        Self {
            label_definitions: settings.label_definitions.clone(),
            tag_definitions: settings.tag_definitions.clone(),
            next_label_id: settings.next_label_id,
            next_tag_id: settings.next_tag_id,
        }
    }
}

fn path(workspace: &Path) -> PathBuf {
    workspace.join(".adbstudio").join("metadata.json")
}

pub fn load(workspace: &Path) -> WorkspaceMetadata {
    let mut metadata: WorkspaceMetadata = fs::read_to_string(path(workspace))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default();
    let mut settings = AppSettings::default();
    metadata.apply_to_settings(&mut settings);
    metadata = WorkspaceMetadata::from_settings(&settings);
    metadata
}

pub fn save(workspace: &Path, settings: &AppSettings) {
    let metadata_path = path(workspace);
    let Some(directory) = metadata_path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_string_pretty(&WorkspaceMetadata::from_settings(settings))
    else {
        return;
    };
    let _ = fs::write(metadata_path, contents);
}

fn default_label_definitions() -> Vec<LabelDefinition> {
    AppSettings::default().label_definitions
}

fn default_tag_definitions() -> Vec<TagDefinition> {
    AppSettings::default().tag_definitions
}

fn default_next_label_id() -> u64 {
    7
}

fn default_next_tag_id() -> u64 {
    7
}

#[cfg(test)]
mod tests {
    use super::{load, save};
    use crate::settings::AppSettings;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn definitions_round_trip_in_workspace_metadata() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("adb-studio-metadata-{suffix}"));
        fs::create_dir_all(&workspace).unwrap();
        let mut settings = AppSettings::default();
        settings.create_tag("Ambient".into());
        save(&workspace, &settings);
        assert_eq!(
            load(&workspace).tag_definitions.last().unwrap().name,
            "Ambient"
        );
        let _ = fs::remove_dir_all(workspace);
    }
}
