use std::process::Command;

pub fn sync_cursor_environment() {
    if std::env::var_os("XCURSOR_THEME").is_none() {
        if let Some(theme) = gsettings_value("org.gnome.desktop.interface", "cursor-theme") {
            std::env::set_var("XCURSOR_THEME", theme);
        }
    }
    if std::env::var_os("XCURSOR_SIZE").is_none() {
        if let Some(size) = gsettings_value("org.gnome.desktop.interface", "cursor-size") {
            std::env::set_var("XCURSOR_SIZE", size);
        }
    }
}

fn gsettings_value(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    Some(value.trim_matches('\'').to_owned())
}
