use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

mod storage;
mod window;

pub use storage::{load, save};
pub use window::{restore as restore_window, save as save_window};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LabelDefinition {
    pub id: u64,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TagDefinition {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AppSettings {
    pub last_folder: Option<String>,
    pub last_selected_path: Option<String>,
    pub light_theme: bool,
    #[serde(
        default,
        alias = "loop_enabled",
        deserialize_with = "deserialize_loop_mode"
    )]
    pub loop_mode: i32,
    #[serde(default)]
    pub auto_play_new_tracks: bool,
    #[serde(default = "default_notification_sound")]
    pub notification_sound: bool,
    #[serde(default = "default_seek_seconds")]
    pub seek_seconds: f32,
    #[serde(default = "default_sort_order")]
    pub sort_order: i32,
    #[serde(default)]
    pub shortcut_fullscreen: i32,
    #[serde(default = "default_shortcut_metadata")]
    pub shortcut_metadata: i32,
    #[serde(default = "default_shortcut_play_pause")]
    pub shortcut_play_pause: i32,
    #[serde(default = "default_shortcut_navigate_up")]
    pub shortcut_navigate_up: i32,
    #[serde(default = "default_shortcut_navigate_down")]
    pub shortcut_navigate_down: i32,
    #[serde(default = "default_shortcut_cancel_edit")]
    pub shortcut_cancel_edit: i32,
    #[serde(default = "default_shortcut_seek_backward")]
    pub shortcut_seek_backward: i32,
    #[serde(default = "default_shortcut_seek_forward")]
    pub shortcut_seek_forward: i32,
    #[serde(default = "default_shortcut_trash")]
    pub shortcut_trash: i32,
    #[serde(default)]
    pub trash_confirmation_disabled_workspaces: HashSet<String>,
    #[serde(default)]
    pub workflow_save_confirmation_disabled_workspaces: HashSet<String>,
    #[serde(default = "default_left_pane_width")]
    pub left_pane_width: f32,
    #[serde(default = "default_metadata_pane_height")]
    pub metadata_pane_height: f32,
    #[serde(default)]
    pub metadata_visible: bool,
    #[serde(default)]
    pub hide_tips_of_the_day: bool,
    #[serde(default = "default_comment_background_color")]
    pub comment_background_color: String,
    #[serde(default = "default_comment_text_color")]
    pub comment_text_color: String,
    #[serde(default = "default_stem_format")]
    pub stem_format: String,
    #[serde(default = "default_stem_output_folder")]
    pub stem_output_folder: String,
    #[serde(skip, default = "default_label_definitions")]
    pub label_definitions: Vec<LabelDefinition>,
    #[serde(skip, default = "default_tag_definitions")]
    pub tag_definitions: Vec<TagDefinition>,
    #[serde(skip, default = "default_next_label_id")]
    pub next_label_id: u64,
    #[serde(skip, default = "default_next_tag_id")]
    pub next_tag_id: u64,
    #[serde(default)]
    pub window_width: Option<u32>,
    #[serde(default)]
    pub window_height: Option<u32>,
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,
    #[serde(default)]
    pub window_maximized: bool,
}

fn default_left_pane_width() -> f32 {
    280.0
}

fn default_metadata_pane_height() -> f32 {
    320.0
}

fn default_seek_seconds() -> f32 {
    5.0
}
fn default_notification_sound() -> bool {
    true
}
fn default_sort_order() -> i32 {
    3
}
fn default_shortcut_metadata() -> i32 {
    1
}
fn default_shortcut_play_pause() -> i32 {
    2
}
fn default_shortcut_navigate_up() -> i32 {
    3
}
fn default_shortcut_navigate_down() -> i32 {
    4
}
fn default_shortcut_cancel_edit() -> i32 {
    5
}
fn default_shortcut_seek_backward() -> i32 {
    7
}
fn default_shortcut_seek_forward() -> i32 {
    8
}
fn default_shortcut_trash() -> i32 {
    9
}

fn default_comment_background_color() -> String {
    "#000000".to_owned()
}

fn default_comment_text_color() -> String {
    "#ffffff".to_owned()
}

fn default_stem_format() -> String {
    "flac".to_owned()
}

fn default_stem_output_folder() -> String {
    "stems".to_owned()
}

fn default_label_definitions() -> Vec<LabelDefinition> {
    vec![
        LabelDefinition {
            id: 1,
            name: "Base idea".to_owned(),
            color: "#a9d8f5".to_owned(),
        },
        LabelDefinition {
            id: 2,
            name: "Good alternative".to_owned(),
            color: "#d5efff".to_owned(),
        },
        LabelDefinition {
            id: 3,
            name: "Bad".to_owned(),
            color: "#f3b1b1".to_owned(),
        },
        LabelDefinition {
            id: 4,
            name: "Use as FX material".to_owned(),
            color: "#d6b8ed".to_owned(),
        },
        LabelDefinition {
            id: 5,
            name: "Could be improved".to_owned(),
            color: "#f4e29c".to_owned(),
        },
        LabelDefinition {
            id: 6,
            name: "Use as sample".to_owned(),
            color: "#b7e4a8".to_owned(),
        },
    ]
}

fn default_tag_definitions() -> Vec<TagDefinition> {
    ["Funk", "Techno", "Electro", "Jazz", "Classical", "Choir"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| TagDefinition {
            id: (index + 1) as u64,
            name: name.to_owned(),
        })
        .collect()
}

fn default_next_label_id() -> u64 {
    7
}

fn default_next_tag_id() -> u64 {
    7
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredLoopMode {
    Mode(i32),
    Legacy(bool),
}

fn deserialize_loop_mode<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    match StoredLoopMode::deserialize(deserializer)? {
        StoredLoopMode::Mode(mode) => Ok(mode.clamp(0, 2)),
        StoredLoopMode::Legacy(enabled) => Ok(i32::from(enabled)),
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_folder: None,
            last_selected_path: None,
            light_theme: false,
            loop_mode: 0,
            auto_play_new_tracks: false,
            notification_sound: default_notification_sound(),
            seek_seconds: default_seek_seconds(),
            sort_order: default_sort_order(),
            shortcut_fullscreen: 0,
            shortcut_metadata: default_shortcut_metadata(),
            shortcut_play_pause: default_shortcut_play_pause(),
            shortcut_navigate_up: default_shortcut_navigate_up(),
            shortcut_navigate_down: default_shortcut_navigate_down(),
            shortcut_cancel_edit: default_shortcut_cancel_edit(),
            shortcut_seek_backward: default_shortcut_seek_backward(),
            shortcut_seek_forward: default_shortcut_seek_forward(),
            shortcut_trash: default_shortcut_trash(),
            trash_confirmation_disabled_workspaces: HashSet::new(),
            workflow_save_confirmation_disabled_workspaces: HashSet::new(),
            left_pane_width: default_left_pane_width(),
            metadata_pane_height: default_metadata_pane_height(),
            metadata_visible: true,
            hide_tips_of_the_day: false,
            comment_background_color: default_comment_background_color(),
            comment_text_color: default_comment_text_color(),
            stem_format: default_stem_format(),
            stem_output_folder: default_stem_output_folder(),
            label_definitions: default_label_definitions(),
            tag_definitions: default_tag_definitions(),
            next_label_id: default_next_label_id(),
            next_tag_id: default_next_tag_id(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: false,
        }
    }
}

impl AppSettings {
    pub fn create_label(&mut self, name: String, color: String) -> u64 {
        let id = self.next_label_id.max(1);
        self.next_label_id = id.saturating_add(1);
        self.label_definitions
            .push(LabelDefinition { id, name, color });
        id
    }

    pub fn create_tag(&mut self, name: String) -> u64 {
        let id = self.next_tag_id.max(1);
        self.next_tag_id = id.saturating_add(1);
        self.tag_definitions.push(TagDefinition { id, name });
        id
    }

    pub fn normalize_next_ids(&mut self) {
        let next_label_id = self
            .label_definitions
            .iter()
            .map(|label| label.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let next_tag_id = self
            .tag_definitions
            .iter()
            .map(|tag| tag.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.next_label_id = self.next_label_id.max(next_label_id);
        self.next_tag_id = self.next_tag_id.max(next_tag_id);
    }
}

pub fn parse_hex_color(value: &str) -> Option<slint::Color> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&value[0..2], 16).ok()?;
    let green = u8::from_str_radix(&value[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
    Some(slint::Color::from_argb_u8(255, red, green, blue))
}

pub fn parse_color(value: &str, fallback: slint::Color) -> slint::Color {
    parse_hex_color(value).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::{parse_color, parse_hex_color, AppSettings};

    #[test]
    fn defaults_are_applied_when_optional_settings_are_missing() {
        let settings: AppSettings = serde_json::from_str("{\"light_theme\":true}").unwrap();

        assert!(settings.light_theme);
        assert_eq!(settings.loop_mode, 0);
        assert_eq!(settings.seek_seconds, 5.0);
        assert_eq!(settings.sort_order, 3);
        assert_eq!(settings.left_pane_width, 280.0);
        assert_eq!(settings.metadata_pane_height, 320.0);
        assert_eq!(settings.comment_background_color, "#000000");
        assert_eq!(settings.comment_text_color, "#ffffff");
        assert_eq!(settings.label_definitions.len(), 6);
        assert_eq!(settings.tag_definitions.len(), 6);
        assert_eq!(settings.next_label_id, 7);
        assert_eq!(settings.next_tag_id, 7);
    }

    #[test]
    fn loop_mode_accepts_legacy_booleans_and_clamps_numbers() {
        let enabled: AppSettings =
            serde_json::from_str("{\"light_theme\":false,\"loop_enabled\":true}").unwrap();
        let clamped: AppSettings =
            serde_json::from_str("{\"light_theme\":false,\"loop_mode\":99}").unwrap();

        assert_eq!(enabled.loop_mode, 1);
        assert_eq!(clamped.loop_mode, 2);
    }

    #[test]
    fn hex_colors_accept_hash_prefix_and_reject_invalid_values() {
        assert_eq!(
            parse_hex_color("#1234ab"),
            Some(slint::Color::from_argb_u8(255, 0x12, 0x34, 0xab))
        );
        assert_eq!(
            parse_hex_color("1234ab"),
            Some(slint::Color::from_argb_u8(255, 0x12, 0x34, 0xab))
        );
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
    }

    #[test]
    fn parse_color_returns_fallback_for_invalid_values() {
        let fallback = slint::Color::from_argb_u8(255, 1, 2, 3);
        assert_eq!(parse_color("not-a-color", fallback), fallback);
    }

    #[test]
    fn created_definition_ids_are_monotonic() {
        let mut settings = AppSettings::default();
        let first_label = settings.create_label("Custom".into(), "#123456".into());
        let second_label = settings.create_label("Another".into(), "#654321".into());
        let first_tag = settings.create_tag("Ambient".into());

        assert_eq!(first_label, 7);
        assert_eq!(second_label, 8);
        assert_eq!(first_tag, 7);
        settings
            .label_definitions
            .retain(|label| label.id != first_label);
        assert_eq!(
            settings.create_label("Recreated".into(), "#abcdef".into()),
            9
        );
    }

    #[test]
    fn next_ids_are_repaired_without_recycling_existing_ids() {
        let mut settings = AppSettings::default();
        settings.label_definitions = vec![super::LabelDefinition {
            id: 42,
            name: "Saved".into(),
            color: "#123456".into(),
        }];
        settings.tag_definitions = vec![super::TagDefinition {
            id: 19,
            name: "Saved tag".into(),
        }];
        settings.normalize_next_ids();

        assert_eq!(settings.create_label("New".into(), "#abcdef".into()), 43);
        assert_eq!(settings.create_tag("New tag".into()), 20);
    }
}
