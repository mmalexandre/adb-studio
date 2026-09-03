use std::{fs::File, path::{Path, PathBuf}, time::Duration};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

pub struct PlaybackEngine {
    stream: MixerDeviceSink,
    player: Option<Player>,
    path: Option<PathBuf>,
    duration: Duration,
    position: Duration,
    playing: bool,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self, String> {
        let stream = DeviceSinkBuilder::open_default_sink()
            .map_err(|error| format!("Could not open audio output: {error}"))?;
        Ok(Self {
            stream,
            player: None,
            path: None,
            duration: Duration::ZERO,
            position: Duration::ZERO,
            playing: false,
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
        player.append(decoder);
        if position > Duration::ZERO {
            player.try_seek(position)
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

    pub fn resume(&mut self) {
        if let Some(player) = &self.player {
            player.play();
            self.playing = true;
        }
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), String> {
        let position = position.min(self.duration);
        if let Some(player) = &self.player {
            player.try_seek(position)
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

    pub fn is_playing(&self) -> bool {
        self.playing && self.player.as_ref().is_some_and(|player| !player.empty())
    }

    pub fn can_resume(&self) -> bool {
        self.player.as_ref().is_some_and(|player| !player.empty())
    }
}
