use std::path::PathBuf;

slint::include_modules!();

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_NUMBER: &str = env!("ADB_BUILD_NUMBER");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MainWindow::new()?;
    window.set_build_number(BUILD_NUMBER.into());

    {
        let weak_window = window.as_weak();
        window.on_open_folder(move || {
            let Some(window) = weak_window.upgrade() else {
                return;
            };

            if let Some(folder) = rfd::FileDialog::new().set_title("Open Adb Studio Workspace").pick_folder() {
                set_workspace(&window, folder);
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

    println!("Adb Studio {APP_VERSION} ({BUILD_NUMBER})");
    window.run()?;
    Ok(())
}

fn set_workspace(window: &MainWindow, folder: PathBuf) {
    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.to_str().unwrap_or("Workspace"));

    window.set_folder_name(folder_name.into());
    window.set_has_folder(true);
}
