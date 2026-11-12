//! Sound: replies are read aloud sentence by sentence while they stream, and one
//! spoken phrase is recorded from the microphone for recognition.
//!
//! The wake phrase is not here: it needs local recognition that listens all the
//! time, which waits for the measurements of spike 0.3.

pub mod mic;
pub mod speech;
pub mod wake;
pub mod win;

pub use mic::{MicControl, MicError, Microphone, SystemMicrophone};
pub use speech::{run_speaker, FirstSound, RemoteSynth, SpeakerSwitch, Voice, VoiceInfo};
