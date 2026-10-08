//! The success and failure sounds, from speechout: one when an action works,
//! the other when it does not, alongside the message in the status bar.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

/// A short sound marking how something went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Success,
    Failure,
}

impl Cue {
    fn bytes(self) -> &'static [u8] {
        match self {
            Cue::Success => include_bytes!("../assets/sounds/success.mp3"),
            Cue::Failure => include_bytes!("../assets/sounds/failure.wav"),
        }
    }
}

/// Plays cues, one at a time, each on its own thread so the window never
/// waits for the audio device. A machine with no audio device just stays
/// quiet; the status bar still says what happened.
#[derive(Default)]
pub struct Cues {
    /// How many cues have been started, and the one now playing. A cue whose
    /// number is out of date by the time its audio device opens is not
    /// played, so a newer cue always replaces an older one.
    current: Arc<Mutex<(u64, Option<Arc<rodio::Player>>)>>,
}

impl Cues {
    pub fn play(&self, cue: Cue) {
        let Some(number) = self.stop() else { return };
        let current = self.current.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut device = rodio::DeviceSinkBuilder::open_default_sink()
                    .map_err(|e| format!("no audio output device is available: {e}"))?;
                device.log_on_drop(false);
                let player = Arc::new(rodio::Player::connect_new(device.mixer()));
                let decoder = rodio::Decoder::try_from(Cursor::new(cue.bytes())).map_err(|e| e.to_string())?;
                {
                    let Ok(mut slot) = current.lock() else { return Ok(()) };
                    if slot.0 != number {
                        return Ok(());
                    }
                    slot.1 = Some(player.clone());
                    player.append(decoder);
                }
                player.sleep_until_end();
                if let Ok(mut slot) = current.lock()
                    && slot.0 == number
                {
                    slot.1 = None;
                }
                Ok(())
            })();
            if let Err(e) = result {
                log::warn!("could not play the {cue:?} sound: {e}");
            }
        });
    }

    /// Cuts off the cue now playing or about to play. Returns the number the
    /// next cue should have.
    fn stop(&self) -> Option<u64> {
        let mut slot = self.current.lock().ok()?;
        slot.0 += 1;
        if let Some(player) = slot.1.take() {
            player.stop();
        }
        Some(slot.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_decode() {
        for cue in [Cue::Success, Cue::Failure] {
            let decoder = rodio::Decoder::try_from(Cursor::new(cue.bytes())).unwrap();
            assert!(decoder.count() > 0, "{cue:?} has no audio");
        }
    }
}
