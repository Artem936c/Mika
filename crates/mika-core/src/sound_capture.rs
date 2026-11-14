//! Writing sound to a file: what the computer plays, or the microphone.
//! Traits only; the devices live in `mika-audio`.

use std::path::Path;

/// Where the sound of a recording comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundSource {
    /// Everything the computer plays, taken from the default output device.
    System,
    /// The microphone chosen in the settings.
    Microphone,
}

/// What a take wrote.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundStats {
    /// Seconds written.
    pub seconds: f64,
    /// Nothing but silence was written.
    pub silent: bool,
}

/// One running recording of sound into a file.
pub trait SoundTake: Send {
    /// Stops and closes the file.
    fn stop(self: Box<Self>) -> Result<SoundStats, String>;
}

pub trait SoundCapture: Send + Sync {
    /// Starts writing `source` to `wav` (PCM 16 bit). The file's time starts at the moment this
    /// returns and follows the clock: while the source gives nothing, silence is written.
    fn start(&self, source: SoundSource, wav: &Path) -> Result<Box<dyn SoundTake>, String>;
}

/// For places without sound devices: every start fails with «запись звука здесь недоступна».
pub struct NoSound;

impl SoundCapture for NoSound {
    fn start(&self, _source: SoundSource, _wav: &Path) -> Result<Box<dyn SoundTake>, String> {
        Err("запись звука здесь недоступна".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sound_refuses_every_start() {
        for source in [SoundSource::System, SoundSource::Microphone] {
            match NoSound.start(source, Path::new("C:\\t\\x.wav")) {
                Err(e) => assert_eq!(e, "запись звука здесь недоступна"),
                Ok(_) => panic!("a take from nowhere"),
            }
        }
    }
}
