//! Sound: replies are read aloud sentence by sentence while they stream, and one
//! spoken phrase is recorded from the microphone for recognition. The wake
//! phrase is heard on this computer, by Vosk or by the recognizer of Windows.

pub mod mic;
pub mod speech;
pub mod vosk;
pub mod wake;
pub mod win;

pub use mic::{MicControl, MicError, Microphone, SystemMicrophone};
pub use speech::{run_speaker, FirstSound, RemoteSynth, SpeakerSwitch, Voice, VoiceInfo};
