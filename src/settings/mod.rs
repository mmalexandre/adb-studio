use serde::{Deserialize, Serialize};

mod storage;
mod window;

pub use storage::{load, save};
pub use window::{restore as restore_window, save as save_window};

#[derive(Clone, Deserialize, Serialize)]
pub struct AppSettings {
    pub last_folder: Option<String>,
    pub last_selected_path: Option<String>,
    pub light_theme: bool,
    #[serde(default)]
    pub loop_enabled: bool,
    #[serde(default)]
    pub auto_play_new_tracks: bool,
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
    190.0
}

fn default_comment_background_color() -> String {
    "#000000".to_owned()
}

fn default_comment_text_color() -> String {
    "#ffffff".to_owned()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_folder: None,
            last_selected_path: None,
            light_theme: false,
            loop_enabled: false,
            auto_play_new_tracks: false,
            left_pane_width: default_left_pane_width(),
            metadata_pane_height: default_metadata_pane_height(),
            metadata_visible: false,
            hide_tips_of_the_day: false,
            comment_background_color: default_comment_background_color(),
            comment_text_color: default_comment_text_color(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: false,
        }
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
