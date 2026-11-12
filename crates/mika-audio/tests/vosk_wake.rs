//! The wake phrase on real sound, without a microphone: Windows voices say
//! phrases with and without the name, and Vosk listens to them.
//!
//! Needs the downloaded Vosk: `MIKA_VOSK_DIR` holds `lib` with `libvosk.dll`
//! and `model` with the small Russian model.
//!
//! ```text
//! set MIKA_VOSK_DIR=%LOCALAPPDATA%\Mika\vosk
//! cargo test -p mika-audio --test vosk_wake -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use mika_audio::wake::{load_vosk, Listener};
use windows::core::HSTRING;
use windows::Media::SpeechSynthesis::SpeechSynthesizer;
use windows::Storage::Streams::DataReader;

/// Mono samples and rate of a phrase said by a Windows voice.
fn say(voice: &str, text: &str) -> (Vec<f32>, u32) {
    let synth = SpeechSynthesizer::new().unwrap();
    let info = SpeechSynthesizer::AllVoices()
        .unwrap()
        .into_iter()
        .find(|v| v.DisplayName().unwrap().to_string().contains(voice))
        .unwrap_or_else(|| panic!("no voice {voice}"));
    synth.SetVoice(&info).unwrap();
    let stream = synth
        .SynthesizeTextToStreamAsync(&HSTRING::from(text))
        .unwrap()
        .join()
        .unwrap();
    let size = stream.Size().unwrap() as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).unwrap()).unwrap();
    reader.LoadAsync(size).unwrap().join().unwrap();
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).unwrap();
    wav_samples(&bytes)
}

/// 16-bit mono WAV: rate and samples of the `data` chunk.
fn wav_samples(wav: &[u8]) -> (Vec<f32>, u32) {
    let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
    let channels = u16::from_le_bytes(wav[22..24].try_into().unwrap()) as usize;
    let mut at = 12;
    while at + 8 <= wav.len() {
        let len = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
        if &wav[at..at + 4] == b"data" {
            let data = &wav[at + 8..(at + 8 + len).min(wav.len())];
            let samples: Vec<f32> = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect();
            return (mika_audio::mic::to_mono(&samples, channels), rate);
        }
        at += 8 + len;
    }
    panic!("no data in the WAV");
}

#[test]
#[ignore = "needs the downloaded Vosk in MIKA_VOSK_DIR"]
fn vosk_hears_the_name_in_any_voice_and_nothing_else() {
    let dir = PathBuf::from(std::env::var_os("MIKA_VOSK_DIR").expect("MIKA_VOSK_DIR"));
    let name = "мика";
    let model = load_vosk(&dir.join("lib"), &dir.join("model"), name).unwrap();

    let wake = [
        "Мика",
        "Мика!",
        "Эй, Мика",
        "Привет, Мика",
        "Мика, открой браузер",
        "Мика, слушай",
    ];
    let other = [
        "Микроволновка опять сломалась",
        "Мама мыла раму",
        "Какая завтра погода",
        "Михаил пришёл домой",
        "Ника, иди сюда",
        "Мы как-нибудь потом",
        "Включи музыку погромче",
        "Миска стоит на столе",
        "Микрофон работает",
        "Мир, труд, май",
    ];
    let mut misses = Vec::new();
    for voice in ["Irina", "Pavel"] {
        for (text, expected) in wake
            .iter()
            .map(|t| (t, true))
            .chain(other.iter().map(|t| (t, false)))
        {
            let (samples, rate) = say(voice, text);
            let mut listener = Listener::new(&model, name, rate).unwrap();
            // Half a second of room before and a second after, as in life.
            let silence = vec![0.0f32; rate as usize / 2];
            let mut heard = listener.feed(&silence);
            heard.extend(listener.feed(&samples));
            heard.extend(listener.feed(&silence));
            heard.extend(listener.feed(&silence));
            // Once per phrase: the cooldown keeps the rest of it quiet.
            let ok = if expected {
                heard.len() == 1
            } else {
                heard.is_empty()
            };
            println!(
                "{} {voice:6} «{text}» -> {heard:?}",
                if ok { "ok  " } else { "MISS" }
            );
            if !ok {
                misses.push(format!("{voice}: {text}"));
            }
        }
    }
    assert!(misses.is_empty(), "wrong: {misses:?}");
}
