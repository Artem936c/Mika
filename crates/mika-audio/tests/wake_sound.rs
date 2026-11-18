//! The wake phrase on real sound, without a microphone: Windows voices say
//! phrases with and without the name, and the downloaded engines listen.
//!
//! Vosk: `MIKA_VOSK_DIR` holds `lib` with `libvosk.dll` and `model` with the
//! small Russian model. sherpa-onnx: `MIKA_SHERPA_DIR` holds `lib` with
//! `sherpa-onnx-c-api.dll` and `model` with the streaming Russian model.
//!
//! ```text
//! set MIKA_VOSK_DIR=%LOCALAPPDATA%\Mika\vosk
//! set MIKA_SHERPA_DIR=%LOCALAPPDATA%\Mika\sherpa
//! cargo test -p mika-audio --test wake_sound -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use mika_audio::sherpa::SherpaLib;
use mika_audio::wake::{load_vosk, only_stop_words, Listener};
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

const WAKE: [&str; 6] = [
    "Мика",
    "Мика!",
    "Эй, Мика",
    "Привет, Мика",
    "Мика, открой браузер",
    "Мика, слушай",
];

const OTHER: [&str; 12] = [
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
    "Стопка книг лежит на полу",
    "Тихий вечер на даче",
];

const STOPS: [&str; 5] = ["Стоп", "Стой", "Хватит", "Останови", "Перестань слушать"];

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Name,
    Stop,
    Nothing,
}

/// Every phrase in both voices through a fresh listener from `make`.
/// Returns the phrases heard wrong.
fn check(make: impl Fn(u32) -> Listener) -> Vec<String> {
    let mut misses = Vec::new();
    for voice in ["Irina", "Pavel"] {
        let phrases = WAKE
            .iter()
            .map(|t| (t, Expect::Name))
            .chain(STOPS.iter().map(|t| (t, Expect::Stop)))
            .chain(OTHER.iter().map(|t| (t, Expect::Nothing)));
        for (text, expected) in phrases {
            let (samples, rate) = say(voice, text);
            let mut listener = make(rate);
            // Half a second of room before and a second after, as in life.
            let silence = vec![0.0f32; rate as usize / 2];
            let mut heard = listener.feed(&silence);
            heard.extend(listener.feed(&samples));
            for _ in 0..4 {
                heard.extend(listener.feed(&silence));
            }
            // Once per phrase: the cooldown keeps the rest of it quiet.
            let ok = match expected {
                Expect::Name => heard.len() == 1 && !only_stop_words(&heard[0]),
                Expect::Stop => heard.len() == 1 && only_stop_words(&heard[0]),
                // A stop word inside a phrase may come; a lone stop or the
                // name may not.
                Expect::Nothing => heard
                    .iter()
                    .all(|h| !only_stop_words(h) && !h.to_lowercase().contains("мика")),
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
    misses
}

#[test]
#[ignore = "needs the downloaded Vosk in MIKA_VOSK_DIR"]
fn vosk_hears_the_name_and_the_stops_in_any_voice_and_nothing_else() {
    let dir = PathBuf::from(std::env::var_os("MIKA_VOSK_DIR").expect("MIKA_VOSK_DIR"));
    let name = "мика";
    let model = load_vosk(&dir.join("lib"), &dir.join("model"), name).unwrap();
    let misses = check(|rate| Listener::new(&model, name, rate).unwrap());
    assert!(misses.is_empty(), "wrong: {misses:?}");
}

#[test]
#[ignore = "needs the downloaded sherpa-onnx in MIKA_SHERPA_DIR"]
fn sherpa_hears_the_name_and_the_stops_in_any_voice_and_nothing_else() {
    let dir = PathBuf::from(std::env::var_os("MIKA_SHERPA_DIR").expect("MIKA_SHERPA_DIR"));
    let lib = SherpaLib::load(&dir.join("lib")).unwrap();
    let model = dir.join("model");
    let misses = check(|rate| Listener::sherpa(lib.clone(), &model, "Мика", rate).unwrap());
    assert!(misses.is_empty(), "wrong: {misses:?}");
}
