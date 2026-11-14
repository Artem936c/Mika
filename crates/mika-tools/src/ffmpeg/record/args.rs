//! Argument lists of a recording, built without running anything.
//!
//! Only numbers and paths made here go to FFmpeg: the area of the screen is
//! given by its rectangle, the input is always the whole desktop, and no
//! window title or text of the model ever becomes an argument.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mika_core::tools::ToolError;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::super::args::{s, Cmd};
use super::super::encoders::H264;
use super::super::locate::file_key;
use super::super::output::ext_of;
use super::super::time::arg_time;
use super::super::FfmpegPaths;
use super::plan::FPS;
use crate::sys::{Rect, Runner};

/// Longest wait for the list of devices.
const DEVICES_TIMEOUT: Duration = Duration::from_secs(15);

/// `ffmpeg -devices`: what this build can capture from.
pub(crate) fn devices_args() -> Vec<String> {
    s(&["-hide_banner", "-devices"])
}

/// Names of the devices FFmpeg can read from: the lines after the ` --`
/// separator whose flags hold `D`.
pub(crate) fn parse_devices(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut listing = false;
    for line in text.lines() {
        if !listing {
            listing = line.trim_start().starts_with("--");
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(flags), Some(names)) = (fields.next(), fields.next()) else {
            continue;
        };
        if flags.len() <= 2 && flags.contains('D') {
            found.extend(names.split(',').map(str::to_string));
        }
    }
    found
}

type Key = (String, u64, std::time::SystemTime);

/// The devices of each FFmpeg, by its path, size and time.
static KNOWN: Mutex<Option<HashMap<Key, BTreeSet<String>>>> = Mutex::new(None);

/// The input devices of this FFmpeg; asked once per file.
pub(crate) async fn devices(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    cancel: &CancellationToken,
) -> Result<BTreeSet<String>, ToolError> {
    let key = file_key(&paths.ffmpeg);
    if let Some(known) = key
        .as_ref()
        .and_then(|k| KNOWN.lock().as_ref().and_then(|all| all.get(k).cloned()))
    {
        return Ok(known);
    }
    let out = runner
        .run(
            &paths.ffmpeg.to_string_lossy(),
            &devices_args(),
            DEVICES_TIMEOUT,
            cancel.clone(),
        )
        .await
        .map_err(|e| {
            if e == "остановлено" {
                ToolError::Cancelled
            } else {
                ToolError::Failed(e)
            }
        })?;
    // Older builds print the list to stderr.
    let mut list = parse_devices(&out.stdout);
    list.extend(parse_devices(&out.stderr));
    if let (Some(k), false) = (key, list.is_empty()) {
        KNOWN
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(k, list.clone());
    }
    Ok(list)
}

/// One piece of the recording: the area `region` of the desktop for at most
/// `limit_s`, encoded to the picture size `frame` into Matroska, which stays
/// readable when the process is killed. No `-nostdin`: `q` on its input
/// ends it.
pub(crate) fn capture_args(
    region: Rect,
    frame: (u32, u32),
    limit_s: f64,
    h264: H264,
    out: &Path,
) -> Vec<String> {
    let (fw, fh) = frame;
    let mut v = s(&[
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.5",
        "-f",
        "gdigrab",
        "-framerate",
    ]);
    v.push(FPS.to_string());
    v.extend(s(&["-draw_mouse", "1", "-offset_x"]));
    v.push(region.x.to_string());
    v.push("-offset_y".into());
    v.push(region.y.to_string());
    v.push("-video_size".into());
    v.push(format!("{}x{}", region.w, region.h));
    v.extend(s(&["-i", "desktop", "-t"]));
    v.push(arg_time(limit_s));
    if (region.w, region.h) != (fw as i32, fh as i32) {
        // A window that changed its size keeps the picture of the first piece.
        v.push("-vf".into());
        v.push(format!(
            "scale={fw}:{fh}:force_original_aspect_ratio=decrease,pad={fw}:{fh}:(ow-iw)/2:(oh-ih)/2"
        ));
    }
    v.extend(h264.live_args(fw, fh, FPS));
    v.extend(s(&["-an", "-f", "matroska", "-n"]));
    v.push(format!("file:{}", out.display()));
    v
}

/// One piece with its sound: the picture is copied up to `keep_s`, the
/// takes are laid against it by their offsets (positive — the head of the
/// sound is cut, negative — the sound is delayed) and mixed into one AAC track.
pub(crate) fn mux_args(
    video: &Path,
    keep_s: f64,
    sounds: &[(PathBuf, f64)],
    out: &Path,
) -> Vec<String> {
    let mut cmd = Cmd::new().input_as("matroska,webm", &[], video);
    for (wav, offset) in sounds {
        let opts = if *offset > 0.0005 {
            vec!["-ss".to_string(), arg_time(*offset)]
        } else if *offset < -0.0005 {
            vec!["-itsoffset".to_string(), arg_time(-*offset)]
        } else {
            Vec::new()
        };
        cmd = cmd.input_as("wav", &opts, wav);
    }
    cmd = cmd
        .arg("-t")
        .arg(arg_time(keep_s))
        .args(&s(&["-map", "0:v:0", "-c:v", "copy"]));
    cmd = match sounds.len() {
        0 => cmd.arg("-an"),
        1 => cmd.args(&s(&["-map", "1:a:0"])),
        n => {
            let pads: String = (1..=n).map(|i| format!("[{i}:a]")).collect();
            cmd.arg("-filter_complex")
                .arg(format!(
                    "{pads}amix=inputs={n}:duration=longest:normalize=0[a]"
                ))
                .args(&s(&["-map", "[a]"]))
        }
    };
    if !sounds.is_empty() {
        // The same sound in every piece, so that the pieces join without encoding.
        cmd = cmd.args(&s(&[
            "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2",
        ]));
    }
    cmd.faststart(&ext_of(out)).output(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = "Devices:
 D. = Demuxing supported
 .E = Muxing supported
 ---
 D  dshow           DirectShow capture
 D  gdigrab         GDI API Windows frame grabber
 D  lavfi           Libavfilter virtual input device
  E sdl,sdl2        SDL2 output device
 DE fbdev,fb        A device that reads and writes
 D  vfwcap          VfW video capture
";

    #[test]
    fn input_devices_are_read_after_the_legend() {
        assert_eq!(devices_args().join(" "), "-hide_banner -devices");
        let d = parse_devices(DEVICES);
        assert!(d.contains("gdigrab") && d.contains("dshow") && d.contains("lavfi"));
        assert!(
            d.contains("fbdev") && d.contains("fb"),
            "two names of one device"
        );
        assert!(!d.contains("sdl") && !d.contains("sdl2"), "output only");
        assert!(!d.contains("="), "legend lines are not devices");
        // The older two-dash separator.
        let old = " D. = Demuxing supported\n --\n D  gdigrab  GDI\n";
        assert!(parse_devices(old).contains("gdigrab"));
        assert!(parse_devices("").is_empty());
        assert!(parse_devices("Unknown option").is_empty());
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn capture_takes_an_area_of_the_desktop_by_numbers() {
        let out = Path::new("C:\\Users\\me\\Videos\\Mika\\з.mika-part\\seg-0.mkv");
        let argv = capture_args(
            rect(0, 0, 1920, 1080),
            (1920, 1080),
            1800.0,
            H264::X264,
            out,
        );
        assert_eq!(
            argv.join(" "),
            "-hide_banner -nostats -loglevel error -progress pipe:1 -stats_period 0.5 \
-f gdigrab -framerate 30 -draw_mouse 1 -offset_x 0 -offset_y 0 -video_size 1920x1080 -i desktop \
-t 1800.000 -c:v libx264 -preset veryfast -tune zerolatency -crf 23 -g 60 -bf 0 -pix_fmt yuv420p \
-an -f matroska -n file:C:\\Users\\me\\Videos\\Mika\\з.mika-part\\seg-0.mkv"
        );
        assert!(!argv.contains(&"-nostdin".to_string()), "q must reach it");
        let input = argv.iter().position(|a| a == "-i").unwrap();
        assert_eq!(argv[input + 1], "desktop", "never a window title");

        // A second monitor to the left: negative offsets.
        let argv = capture_args(
            rect(-1280, 56, 1280, 1024),
            (1280, 1024),
            60.5,
            H264::Qsv,
            out,
        );
        let line = argv.join(" ");
        assert!(
            line.contains("-offset_x -1280 -offset_y 56 -video_size 1280x1024 -i desktop -t 60.500 -c:v h264_qsv"),
            "{line}"
        );
        assert!(!line.contains("-vf"), "{line}");

        // A window that grew: the piece is fitted into the first picture.
        let argv = capture_args(rect(100, 50, 1000, 700), (800, 600), 10.0, H264::X264, out);
        let line = argv.join(" ");
        assert!(
            line.contains(
                "-video_size 1000x700 -i desktop -t 10.000 \
-vf scale=800:600:force_original_aspect_ratio=decrease,pad=800:600:(ow-iw)/2:(oh-ih)/2 -c:v libx264"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_piece_is_put_together_with_its_sound() {
        let video = Path::new("C:\\w\\seg-0.mkv");
        let sys = PathBuf::from("C:\\w\\seg-0-sys.wav");
        let mic = PathBuf::from("C:\\w\\seg-0-mic.wav");
        let head =
            "-nostdin -hide_banner -nostats -loglevel error -progress pipe:1 -stats_period 0.5 \
-format_whitelist matroska,webm -protocol_whitelist file -i file:C:\\w\\seg-0.mkv";

        // No sound at all: the picture alone, straight into the MP4.
        let argv = mux_args(video, 12.5, &[], Path::new("C:\\v\\з.mika-part.mp4"));
        assert_eq!(
            argv.join(" "),
            format!(
                "{head} -t 12.500 -map 0:v:0 -c:v copy -an -movflags +faststart -n file:C:\\v\\з.mika-part.mp4"
            )
        );

        // One take that began 0.4 s before the first frame.
        let argv = mux_args(
            video,
            12.5,
            &[(sys.clone(), 0.4)],
            Path::new("C:\\w\\part-0.mkv"),
        );
        assert_eq!(
            argv.join(" "),
            format!(
                "{head} -format_whitelist wav -ss 0.400 -protocol_whitelist file -i file:C:\\w\\seg-0-sys.wav \
-t 12.500 -map 0:v:0 -c:v copy -map 1:a:0 -c:a aac -b:a 160k -ar 48000 -ac 2 -n file:C:\\w\\part-0.mkv"
            )
        );

        // Two takes are mixed; the microphone began after the picture.
        let argv = mux_args(
            video,
            9.25,
            &[(sys, 0.4), (mic, -0.15)],
            Path::new("C:\\v\\з.mika-part.mp4"),
        );
        assert_eq!(
            argv.join(" "),
            format!(
                "{head} -format_whitelist wav -ss 0.400 -protocol_whitelist file -i file:C:\\w\\seg-0-sys.wav \
-format_whitelist wav -itsoffset 0.150 -protocol_whitelist file -i file:C:\\w\\seg-0-mic.wav \
-t 9.250 -map 0:v:0 -c:v copy \
-filter_complex [1:a][2:a]amix=inputs=2:duration=longest:normalize=0[a] -map [a] \
-c:a aac -b:a 160k -ar 48000 -ac 2 -movflags +faststart -n file:C:\\v\\з.mika-part.mp4"
            )
        );

        // A take that is on time needs no shift.
        let argv = mux_args(
            video,
            1.0,
            &[(PathBuf::from("C:\\w\\a.wav"), 0.0)],
            Path::new("C:\\w\\p.mkv"),
        );
        let line = argv.join(" ");
        assert!(
            line.contains("-format_whitelist wav -protocol_whitelist file -i file:C:\\w\\a.wav"),
            "{line}"
        );
    }
}
