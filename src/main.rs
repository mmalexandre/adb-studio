use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
};

use serde::{Deserialize, Serialize};

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");

#[derive(Clone, Default, Deserialize, Serialize)]
struct AppSettings {
    last_folder: Option<String>,
    light_theme: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    let settings = Rc::new(RefCell::new(load_settings()));
    window.set_build_number(BUILD_NUMBER.into());
    window.set_light_theme(settings.borrow().light_theme);

    let last_folder = settings.borrow().last_folder.clone();
    if let Some(last_folder) = last_folder {
        let folder = PathBuf::from(last_folder);
        if folder.is_dir() {
            set_workspace(&window, folder, &settings);
        }
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_open_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };

            if let Some(folder) = rfd::FileDialog::new().set_title("Open Adb Studio Workspace").pick_folder() {
                set_workspace(&window, folder, &settings);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_about(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_about_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        window.on_close_settings(move || {
            if let Some(window) = weak_window.upgrade() {
                window.set_settings_visible(false);
            }
        });
    }

    {
        let weak_window = window.as_weak();
        let settings = Rc::clone(&settings);
        window.on_theme_selected(move |light_theme| {
            {
                settings.borrow_mut().light_theme = light_theme;
            }
            let settings_snapshot = settings.borrow().clone();
            save_settings(&settings_snapshot);
            if let Some(window) = weak_window.upgrade() {
                window.set_light_theme(light_theme);
            }
        });
    }

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
    Ok(())
}

fn set_workspace(window: &MainWindow, folder: PathBuf, settings: &Rc<RefCell<AppSettings>>) {
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"));

    window.set_folder_name(folder_name.into());
    window.set_has_folder(true);

    {
        settings.borrow_mut().last_folder = Some(folder.to_string_lossy().into_owned());
    }
    let settings_snapshot = settings.borrow().clone();
    save_settings(&settings_snapshot);
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("adb-studio").join("settings.json"))
}

fn load_settings() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };

    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };

    if fs::create_dir_all(directory).is_err() {
        return;
    }

    let Ok(contents) = serde_json::to_string_pretty(settings) else {
        return;
    };

    let _ = fs::write(path, contents);
}
