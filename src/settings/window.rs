use display_info::DisplayInfo;
use slint::ComponentHandle;

use crate::MainWindow;

use super::AppSettings;

pub fn restore(window: &MainWindow, settings: &mut AppSettings) {
    let (Some(width), Some(height), Some(x), Some(y)) = (
        settings.window_width,
        settings.window_height,
        settings.window_x,
        settings.window_y,
    ) else {
        return;
    };

    let position_is_visible = DisplayInfo::all().map_or(false, |displays| {
        displays.iter().any(|display| {
            x >= display.x
                && y >= display.y
                && i64::from(x) < i64::from(display.x) + i64::from(display.width)
                && i64::from(y) < i64::from(display.y) + i64::from(display.height)
        })
    });
    if !position_is_visible {
        settings.window_width = None;
        settings.window_height = None;
        settings.window_x = None;
        settings.window_y = None;
        settings.window_maximized = false;
        return;
    }
    window
        .window()
        .set_size(slint::PhysicalSize::new(width, height));
    window
        .window()
        .set_position(slint::PhysicalPosition::new(x, y));
    window.window().set_maximized(settings.window_maximized);
}

pub fn save(window: &MainWindow, settings: &mut AppSettings) {
    let size = window.window().size();
    let position = window.window().position();
    settings.window_width = Some(size.width);
    settings.window_height = Some(size.height);
    settings.window_x = Some(position.x);
    settings.window_y = Some(position.y);
    settings.window_maximized = window.window().is_maximized();
}
