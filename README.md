# Adb Studio

A cross-platform Slint + Rust application for annotating Ace Step 1.5 AI-generated music.

## Status

Early development. The current shell includes the welcome screen, workspace folder picker, explorer layout, cached waveform audio library, and About dialog.

## Run

Install Rust, then from the project directory:

```sh
cargo run
```

For Linux, Slint may require Fontconfig development files:

```sh
sudo apt install libfontconfig1-dev
```

## Linux desktop entry

To make GNOME list and group the application as `Adb Studio`, install the release binary and desktop metadata in a user-local prefix:

```sh
cargo build --release
install -Dm755 target/release/adb-studio "$HOME/.local/bin/adb-studio"
install -Dm644 packaging/com.adbstudio.AdbStudio.desktop \
	"$HOME/.local/share/applications/com.adbstudio.AdbStudio.desktop"
install -Dm644 assets/icons/com.adbstudio.AdbStudio.svg \
	"$HOME/.local/share/icons/hicolor/scalable/apps/com.adbstudio.AdbStudio.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
```

Make sure `$HOME/.local/bin` is on `PATH`, then launch `Adb Studio` from GNOME. `cargo run` remains available for development launches.

## License

Not specified yet.
