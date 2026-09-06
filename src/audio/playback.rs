use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

pub struct PlaybackEngine {
    stream: MixerDeviceSink,
    player: Option<Player>,
    volume: f32,
    path: Option<PathBuf>,
    duration: Duration,
    position: Duration,
    playing: bool,
    comment_loop: Option<(Duration, Duration)>,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, String> {
        let stream = DeviceSinkBuilder::open_default_sink()
            .map_err(|error| format!("Could not open audio output: {error}"))?;
        Ok(Self {
            stream,
            player: None,
            volume: 1.0,
            path: None,
            duration: Duration::ZERO,
            position: Duration::ZERO,
            playing: false,
            comment_loop: None,
        })
    }

    pub fn play(&mut self, path: &Path, position: Duration) -> Result<(), String> {
        let file =
            File::open(path).map_err(|error| format!("Could not open audio file: {error}"))?;
        let decoder = Decoder::try_from(file)
            .map_err(|error| format!("Could not decode audio file: {error}"))?;
        let duration = decoder.total_duration().unwrap_or(Duration::ZERO);
        let position = position.min(duration);
        let player = Player::connect_new(self.stream.mixer());
        player.set_volume(self.volume);
        player.append(decoder);
        if position > Duration::ZERO {
            player
                .try_seek(position)
                .map_err(|error| format!("Could not seek audio file: {error}"))?;
        }
        player.play();
        if let Some(previous) = self.player.take() {
            previous.stop();
        }
        self.player = Some(player);
        self.path = Some(path.to_path_buf());
        self.duration = duration;
        self.position = position;
        self.playing = true;
        Ok(())
    }

    pub fn pause(&mut self) {
        self.update_position();
        if let Some(player) = &self.player {
            player.pause();
        }
        self.playing = false;
    }

    pub fn stop(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.path = None;
        self.duration = Duration::ZERO;
        self.position = Duration::ZERO;
        self.playing = false;
        self.comment_loop = None;
    }

    pub fn resume(&mut self) {
        if let Some(player) = &self.player {
            player.play();
            self.playing = true;
        }
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), String> {
        let position = position.min(self.duration);
        if self.player.as_ref().is_some_and(|player| player.empty()) {
            let Some(path) = self.path.clone() else {
                return Ok(());
            };
            return self.play(&path, position);
        }
        if let Some(player) = &self.player {
            player
                .try_seek(position)
                .map_err(|error| format!("Could not seek audio file: {error}"))?;
            self.position = position;
        }
        Ok(())
    }

    pub fn update_position(&mut self) {
        let Some(player) = &self.player else {
            return;
        };
        self.position = player.get_pos().min(self.duration);
        self.playing = !player.empty() && !player.is_paused();
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn set_comment_loop(&mut self, start: Duration, end: Duration) {
        self.comment_loop = Some((start.min(end), end.min(self.duration)));
    }

    pub fn clear_comment_loop(&mut self) {
        self.comment_loop = None;
    }

    pub fn comment_loop(&self) -> Option<(Duration, Duration)> {
        self.comment_loop
    }

    pub fn is_playing(&self) -> bool {
        self.playing && self.player.as_ref().is_some_and(|player| !player.empty())
    }

    pub fn can_resume(&self) -> bool {
        self.player.as_ref().is_some_and(|player| !player.empty())
    }

    pub fn has_finished(&self) -> bool {
        self.duration > Duration::ZERO && self.player.as_ref().is_some_and(|player| player.empty())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(player) = &self.player {
            player.set_volume(self.volume);
        }
    }
}
