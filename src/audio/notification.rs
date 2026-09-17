use std::time::Duration;

use rodio::{source::SineWave, DeviceSinkBuilder, Player, Source};

pub fn play_download_bell() {
    std::thread::spawn(|| {
        let Ok(stream) = DeviceSinkBuilder::open_default_sink() else {
            return;
        };
        let player = Player::connect_new(stream.mixer());
        player.append(
            SineWave::new(880.0)
                .take_duration(Duration::from_millis(120))
                .amplify(0.18),
        );
        player.sleep_until_end();
    });
}
