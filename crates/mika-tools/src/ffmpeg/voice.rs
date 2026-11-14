//! Sound for voice messages through FFmpeg (decision 0029): not a tool, it
//! works on its own temporary files only.
//!
//! The rules of decision 0027 hold here too: the input is read through the
//! `file` protocol by the one demuxer its first bytes name, FFmpeg gets no
//! shell and no stdin, and nothing is written over a file.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::sound::{SoundConverter, SoundKind};
use mika_core::tools::Progress;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

use super::args::{s, Cmd};
use super::job::run_ffmpeg;
use super::locate;
use super::output::WorkDir;
use crate::sys::Runner;

/// Longest conversion.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Longest sound turned into WAV for recognition, seconds: the longest voice
/// message taken (two minutes) and a little over.
const WAV_MAX_S: &str = "125.000";

/// Sound for voice messages through FFmpeg: the user's own, one from `PATH`
/// or the downloaded one, looked for at every call.
pub struct FfmpegSound {
    runner: Arc<dyn Runner>,
    /// `tools.ffmpeg_path` from the settings.
    ffmpeg_path: Arc<RwLock<String>>,
}

impl FfmpegSound {
    pub fn new(runner: Arc<dyn Runner>, ffmpeg_path: Arc<RwLock<String>>) -> Self {
        Self {
            runner,
            ffmpeg_path,
        }
    }

    /// Writes `sound` into a temporary folder, runs FFmpeg with the
    /// arguments `args` builds, and reads `out_name` back. The folder goes
    /// away afterwards.
    async fn convert(
        &self,
        sound: &[u8],
        out_name: &str,
        args: fn(&Path, &str, &Path) -> Vec<String>,
    ) -> Result<Vec<u8>, String> {
        let setting = self.ffmpeg_path.read().clone();
        let Some(paths) = locate(&setting) else {
            return Err("FFmpeg не найден".into());
        };
        let kind = SoundKind::of(sound);
        // Sound of no known kind is not given to FFmpeg at all.
        let Some(demuxer) = demuxer(kind) else {
            return Err("неизвестный формат звука".into());
        };
        let work = WorkDir::new().map_err(|e| e.to_string())?;
        let input = work.path().join(format!("in.{}", kind.extension()));
        let out = work.path().join(out_name);
        std::fs::write(&input, sound)
            .map_err(|e| format!("не удалось записать временный файл: {e}"))?;
        run_ffmpeg(
            self.runner.as_ref(),
            &paths,
            &args(&input, demuxer, &out),
            None,
            0.0,
            (0, 100),
            &Progress::default(),
            &CancellationToken::new(),
            TIMEOUT,
        )
        .await
        .map_err(|e| e.to_string())?;
        match std::fs::read(&out) {
            Ok(bytes) if !bytes.is_empty() => Ok(bytes),
            _ => Err("FFmpeg не записал звук".into()),
        }
    }
}

#[async_trait]
impl SoundConverter for FfmpegSound {
    /// File checks only: no process is started.
    fn ready(&self) -> bool {
        locate(&self.ffmpeg_path.read()).is_some()
    }

    async fn to_wav16k(&self, sound: &[u8]) -> Result<Vec<u8>, String> {
        self.convert(sound, "out.wav", wav16k_args).await
    }

    async fn to_ogg_opus(&self, sound: &[u8]) -> Result<Vec<u8>, String> {
        self.convert(sound, "out.ogg", ogg_opus_args).await
    }
}

/// The one demuxer that may open a sound of this kind.
pub(crate) fn demuxer(kind: SoundKind) -> Option<&'static str> {
    match kind {
        SoundKind::OggOpus | SoundKind::Ogg => Some("ogg"),
        SoundKind::Mp3 => Some("mp3"),
        SoundKind::Wav => Some("wav"),
        SoundKind::Flac => Some("flac"),
        SoundKind::M4a => Some("mov"),
        SoundKind::Unknown => None,
    }
}

/// Any known sound as WAV, 16 kHz, mono, 16 bit: what the microphone sends
/// to recognition.
pub(crate) fn wav16k_args(input: &Path, demuxer: &str, out: &Path) -> Vec<String> {
    Cmd::new()
        .input_as(demuxer, &s(&["-t", WAV_MAX_S]), input)
        .args(&s(&[
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
        ]))
        .output(out)
}

/// Any known sound as OGG/Opus, mono, 32 kbit/s: a voice message.
pub(crate) fn ogg_opus_args(input: &Path, demuxer: &str, out: &Path) -> Vec<String> {
    Cmd::new()
        .input_as(demuxer, &[], input)
        .args(&s(&[
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
            "-application",
            "voip",
        ]))
        .output(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::testkit::{av, Av};
    use crate::sys::RunOutput;

    const PREFIX: &str =
        "-nostdin -hide_banner -nostats -loglevel error -progress pipe:1 -stats_period 0.5";

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    fn sound(a: &Av) -> FfmpegSound {
        FfmpegSound::new(a.env.runner.clone(), a.env.shared.ffmpeg_path.clone())
    }

    /// The start of a Telegram voice message.
    fn ogg_opus() -> Vec<u8> {
        let mut page = b"OggS\0\x02".to_vec();
        page.extend_from_slice(&[0; 20]);
        page.extend_from_slice(&[1, 19]);
        page.extend_from_slice(b"OpusHead\x01\x01\0\0\x80\xbb\0\0\0\0\0");
        page.extend_from_slice(b"voice");
        page
    }

    const WAV: &[u8] = b"RIFF\x24\0\0\0WAVEfmt sound";

    /// Waits until the temporary folder of a run is gone.
    fn gone(folder: &Path) -> bool {
        for _ in 0..100 {
            if !folder.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The input file FFmpeg was given in run `n`.
    fn input_of(a: &Av, n: usize) -> std::path::PathBuf {
        let run = &a.ffmpeg_runs()[n];
        let at = run.iter().position(|x| x == "-i").unwrap();
        run[at + 1].strip_prefix("file:").unwrap().into()
    }

    #[test]
    fn the_sound_for_recognition_is_what_the_microphone_sends() {
        assert_eq!(
            wav16k_args(p("C:\\T\\in.ogg"), "ogg", p("C:\\T\\out.wav")).join(" "),
            format!("{PREFIX} -format_whitelist ogg -t 125.000 -protocol_whitelist file -i file:C:\\T\\in.ogg -vn -sn -dn -ac 1 -ar 16000 -c:a pcm_s16le -n file:C:\\T\\out.wav")
        );
    }

    #[test]
    fn a_voice_message_is_mono_opus_in_ogg() {
        assert_eq!(
            ogg_opus_args(p("C:\\T\\in.wav"), "wav", p("C:\\T\\out.ogg")).join(" "),
            format!("{PREFIX} -format_whitelist wav -protocol_whitelist file -i file:C:\\T\\in.wav -vn -sn -dn -ac 1 -ar 48000 -c:a libopus -b:a 32k -application voip -n file:C:\\T\\out.ogg")
        );
    }

    #[test]
    fn each_kind_of_sound_is_opened_by_its_own_demuxer_only() {
        assert_eq!(demuxer(SoundKind::OggOpus), Some("ogg"));
        assert_eq!(demuxer(SoundKind::Ogg), Some("ogg"));
        assert_eq!(demuxer(SoundKind::Mp3), Some("mp3"));
        assert_eq!(demuxer(SoundKind::Wav), Some("wav"));
        assert_eq!(demuxer(SoundKind::Flac), Some("flac"));
        assert_eq!(demuxer(SoundKind::M4a), Some("mov"));
        assert_eq!(demuxer(SoundKind::Unknown), None);
        // Every one of them is a demuxer the media tools may open as well.
        let media: Vec<&str> = super::super::args::DEMUXERS.split(',').collect();
        for kind in [
            SoundKind::OggOpus,
            SoundKind::Mp3,
            SoundKind::Wav,
            SoundKind::Flac,
            SoundKind::M4a,
        ] {
            let name = demuxer(kind).unwrap();
            assert!(!name.contains(','), "one format only: {name}");
            assert!(media.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_converter_is_ready_when_ffmpeg_is_found() {
        let a = av();
        let sound = sound(&a);
        assert!(sound.ready());
        assert!(a.f.runs.lock().is_empty(), "no process is started to look");
        // The setting is read at every call.
        let nowhere = a.dir.path().join("нет такой папки").display().to_string();
        *a.env.shared.ffmpeg_path.write() = nowhere.clone();
        // PATH or the download folder of this machine may hold one.
        assert_eq!(sound.ready(), locate(&nowhere).is_some());
    }

    #[tokio::test]
    async fn a_voice_message_becomes_wav_in_a_folder_that_goes_away() {
        let a = av();
        *a.f.run_write.lock() = Some(WAV.to_vec());
        let ogg = ogg_opus();
        assert_eq!(sound(&a).to_wav16k(&ogg).await, Ok(WAV.to_vec()));

        let runs = a.ffmpeg_runs();
        assert_eq!(runs.len(), 1);
        let input = input_of(&a, 0);
        let folder = input.parent().unwrap().to_path_buf();
        assert_eq!(input.file_name().unwrap(), "in.ogg");
        assert!(folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("mika-av-"));
        assert_eq!(runs[0], wav16k_args(&input, "ogg", &folder.join("out.wav")));
        // No working folder is given and the run is short.
        assert_eq!(*a.f.run_cwds.lock(), vec![None]);
        assert_eq!(a.f.runs.lock()[0].2, Duration::from_secs(60));
        assert!(gone(&folder), "the temporary folder stays");
    }

    #[tokio::test]
    async fn a_reply_in_wav_becomes_a_voice_message() {
        let a = av();
        *a.f.run_write.lock() = Some(ogg_opus());
        assert_eq!(sound(&a).to_ogg_opus(WAV).await, Ok(ogg_opus()));
        let input = input_of(&a, 0);
        let folder = input.parent().unwrap().to_path_buf();
        assert_eq!(input.file_name().unwrap(), "in.wav");
        assert_eq!(
            a.ffmpeg_runs()[0],
            ogg_opus_args(&input, "wav", &folder.join("out.ogg"))
        );
        assert!(gone(&folder), "the temporary folder stays");
    }

    #[tokio::test]
    async fn an_unknown_sound_never_starts_ffmpeg() {
        let a = av();
        let sound = sound(&a);
        for bytes in [b"no sound here".as_slice(), b"", &[0xFF, 0xFF, 0x01, 0x00]] {
            assert_eq!(
                sound.to_wav16k(bytes).await,
                Err("неизвестный формат звука".into())
            );
            assert_eq!(
                sound.to_ogg_opus(bytes).await,
                Err("неизвестный формат звука".into())
            );
        }
        assert!(a.f.runs.lock().is_empty());
    }

    #[tokio::test]
    async fn a_failed_run_gives_the_complaint_of_ffmpeg() {
        let a = av();
        *a.f.run_write.lock() = None;
        a.f.run_outputs.lock().push_back(RunOutput {
            code: Some(1),
            stderr: "Unknown encoder 'libopus'\n".into(),
            ..Default::default()
        });
        let sound = sound(&a);
        assert_eq!(
            sound.to_ogg_opus(WAV).await,
            Err("FFmpeg завершился с ошибкой: Unknown encoder 'libopus'".into())
        );
        assert!(gone(input_of(&a, 0).parent().unwrap()));

        // A run that ends well and writes nothing is a failure too.
        assert_eq!(
            sound.to_wav16k(&ogg_opus()).await,
            Err("FFmpeg не записал звук".into())
        );
        *a.f.run_write.lock() = Some(Vec::new());
        assert_eq!(
            sound.to_wav16k(&ogg_opus()).await,
            Err("FFmpeg не записал звук".into())
        );
        // One that runs out of time.
        a.f.run_outputs.lock().push_back(RunOutput {
            timed_out: true,
            ..Default::default()
        });
        assert_eq!(
            sound.to_wav16k(&ogg_opus()).await,
            Err("FFmpeg не уложился в срок".into())
        );
    }

    #[tokio::test]
    async fn without_ffmpeg_nothing_is_converted() {
        let a = av();
        let nowhere = a.dir.path().join("нет такой папки").display().to_string();
        *a.env.shared.ffmpeg_path.write() = nowhere.clone();
        // PATH or the download folder of this machine may hold one.
        if locate(&nowhere).is_some() {
            return;
        }
        let sound = sound(&a);
        assert!(!sound.ready());
        assert_eq!(
            sound.to_wav16k(&ogg_opus()).await,
            Err("FFmpeg не найден".into())
        );
        assert_eq!(sound.to_ogg_opus(WAV).await, Err("FFmpeg не найден".into()));
        assert!(a.f.runs.lock().is_empty());
    }
}
