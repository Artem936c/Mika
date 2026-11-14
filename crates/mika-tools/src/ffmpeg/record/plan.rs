//! What a recording is asked to do and how it ended: the parts that need no
//! process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::output::stem;
use super::super::size_text;
use super::super::time::show_time;
use crate::batch::free_name;
use crate::sys::Rect;

/// The tool, and the kind of its background job.
pub(crate) const KIND: &str = "screen_record";
/// Length limit when none is asked for, and the longest one.
pub(crate) const DEFAULT_MINUTES: f64 = 30.0;
pub(crate) const MAX_MINUTES: f64 = 120.0;
pub(crate) const FPS: u32 = 30;
/// How often the window in front and the switches are looked at.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// Cut from the end of a piece that a blind-zone window stopped: frames
/// from before the check noticed it.
pub(crate) const TRIM_TAIL_S: f64 = 0.75;
/// How long FFmpeg gets to end by itself after `q`.
pub(crate) const QUIT_WAIT: Duration = Duration::from_secs(10);
/// The least time a stopped recording gets to put its file together; a long
/// one gets more (`stop_grace`).
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(180);
pub(crate) const MIN_FREE_START_GB: f64 = 2.0;
pub(crate) const MIN_FREE_RUN_GB: f64 = 0.5;
/// A recording that has waited this long past its limit ends.
pub(crate) const PAUSE_CAP_S: f64 = 30.0 * 60.0;
/// The smallest side of a picture worth recording.
pub(crate) const MIN_SIDE: i32 = 64;
/// A piece shorter than this is dropped.
const MIN_KEEP_S: f64 = 0.3;

/// What is recorded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Source {
    /// A whole monitor, counted from 1.
    Screen { monitor: usize },
    /// The place of a window, while its program is in front.
    Window {
        id: u64,
        title: String,
        process: String,
    },
}

/// One recording, as the confirm card shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecPlan {
    pub source: Source,
    /// Screen area in physical pixels, even sides.
    pub region: Rect,
    pub limit_s: f64,
    /// The sound of the computer is recorded.
    pub sound: bool,
    pub microphone: bool,
    pub output: PathBuf,
}

/// The box around all monitors.
pub(crate) fn desktop_of(monitors: &[Rect]) -> Rect {
    let Some(first) = monitors.first() else {
        return Rect::default();
    };
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
    for m in monitors {
        x0 = x0.min(m.x);
        y0 = y0.min(m.y);
        x1 = x1.max(m.x + m.w);
        y1 = y1.max(m.y + m.h);
    }
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// `r` clipped to `desktop`, its width and height rounded down to even
/// numbers as H.264 needs; `None` when a side is under `MIN_SIDE`.
pub(crate) fn even_region(r: Rect, desktop: Rect) -> Option<Rect> {
    let x0 = r.x.max(desktop.x);
    let y0 = r.y.max(desktop.y);
    let x1 = (r.x + r.w).min(desktop.x + desktop.w);
    let y1 = (r.y + r.h).min(desktop.y + desktop.h);
    let w = (x1 - x0).max(0) & !1;
    let h = (y1 - y0).max(0) & !1;
    (w >= MIN_SIDE && h >= MIN_SIDE).then_some(Rect { x: x0, y: y0, w, h })
}

/// The folder recordings go to: `<profile>\Videos\Mika`.
pub(crate) fn videos_dir(profile: &str) -> PathBuf {
    Path::new(profile).join("Videos").join("Mika")
}

/// `<profile>\Videos\Mika\Запись экрана 2026-11-14 15-30-05.mp4`; a name
/// that is taken is counted up.
pub(crate) fn output_path(profile: &str, now: chrono::NaiveDateTime) -> PathBuf {
    let name = format!("Запись экрана {}.mp4", now.format("%Y-%m-%d %H-%M-%S"));
    free_name(&videos_dir(profile).join(name))
}

/// The folder for the pieces of a recording: `<stem>.mika-part` beside it.
pub(crate) fn work_dir(output: &Path) -> PathBuf {
    let dir = output.parent().unwrap_or(Path::new(""));
    free_name(&dir.join(format!("{}.mika-part", stem(output))))
}

/// How the sound of a take lies against the picture, both times counted
/// from one moment: positive — cut that much from the head of the sound (it
/// started before the first frame), negative — delay the sound by that much.
pub(crate) fn sound_offset(video_first_s: f64, take_start_s: f64) -> f64 {
    video_first_s - take_start_s
}

/// Seconds of a piece that go into the file; `None` when too little is left
/// to keep. `blind_at` is how much FFmpeg had written when a blind-zone
/// window was noticed: the piece is cut `TRIM_TAIL_S` before that moment,
/// however long FFmpeg went on writing until it ended.
pub(crate) fn keep_seconds(video_s: f64, blind_at: Option<f64>) -> Option<f64> {
    let keep = match blind_at {
        Some(at) => video_s.min(at) - TRIM_TAIL_S,
        None => video_s,
    };
    (keep.is_finite() && keep >= MIN_KEEP_S).then_some(keep)
}

/// Whether two screen areas share any pixel.
pub(crate) fn overlaps(a: Rect, b: Rect) -> bool {
    a.w > 0
        && a.h > 0
        && b.w > 0
        && b.h > 0
        && a.x < b.x + b.w
        && b.x < a.x + a.w
        && a.y < b.y + b.h
        && b.y < a.y + a.h
}

/// How long a stopped recording of at most `limit_s` may put its file
/// together before its work is dropped: the pieces are copied and their
/// sound is encoded, which for hours of recording takes minutes. Every run
/// of FFmpeg has its own, shorter limit; this one only ends work that hangs.
pub(crate) fn stop_grace(limit_s: f64) -> Duration {
    let copy = 60.0 + limit_s.max(0.0) / 4.0;
    STOP_GRACE.max(Duration::from_secs_f64(60.0 + 4.0 * copy))
}

/// Why a recording ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Stop {
    /// It was asked to: the tool, the button, the tray, the exit of the app.
    User,
    Limit,
    Disk,
    ScreenOff,
    WindowGone,
    PausedTooLong,
    /// The capture ended by itself; holds why.
    Broke(String),
}

/// `1 раз`, `2 раза`, `5 раз`.
fn times_word(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (_, 11..=14) => "раз",
        (2..=4, _) => "раза",
        _ => "раз",
    }
}

/// `30 минут`, `1 минуты`, `1,5 минуты`: the words after «до».
pub(crate) fn minutes_text(minutes: f64) -> String {
    let whole = minutes.fract() == 0.0;
    let n = minutes as u64;
    let word = if !whole || (n % 10 == 1 && n % 100 != 11) {
        "минуты"
    } else {
        "минут"
    };
    format!(
        "{} {word}",
        super::super::args::number(minutes).replace('.', ",")
    )
}

/// What is told when the recording is saved.
pub(crate) fn done_text(
    path: &Path,
    bytes: u64,
    length_s: f64,
    limit_s: f64,
    pauses: u32,
    stop: &Stop,
    sound_note: Option<&str>,
) -> String {
    let mut text = format!(
        "Запись сохранена: {}, {}, {}.",
        path.display(),
        size_text(bytes),
        show_time(length_s)
    );
    if pauses > 0 {
        text.push_str(&format!(
            " Запись {pauses} {} вставала на паузу из-за окна из слепой зоны: эти места в файл не попали.",
            times_word(pauses)
        ));
    }
    let why = match stop {
        Stop::User => None,
        Stop::Limit => Some(format!(
            "Остановлена: достигнут предел {}.",
            minutes_text(limit_s / 60.0)
        )),
        Stop::Disk => Some("Остановлена: на диске мало места.".to_string()),
        Stop::ScreenOff => Some("Остановлена: выключен просмотр экрана.".to_string()),
        Stop::WindowGone => Some("Остановлена: окно закрыто.".to_string()),
        Stop::PausedTooLong => Some("Остановлена: пауза затянулась.".to_string()),
        Stop::Broke(why) => Some(format!(
            "Запись оборвалась: {}.",
            why.trim().trim_end_matches('.')
        )),
    };
    if let Some(why) = why {
        text.push(' ');
        text.push_str(&why);
    }
    if let Some(note) = sound_note.map(str::trim).filter(|n| !n.is_empty()) {
        let mut chars = note.chars();
        let first: String = chars
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .collect();
        text.push_str(&format!(
            " {first}{}.",
            chars.as_str().trim_end_matches('.')
        ));
    }
    text
}

/// Why nothing was recorded, for a recording that leaves no file.
pub(crate) fn nothing_text(stop: &Stop) -> String {
    match stop {
        Stop::User => "запись остановлена сразу".into(),
        Stop::Limit => "предел длины слишком мал".into(),
        Stop::Disk => "на диске мало места".into(),
        Stop::ScreenOff => "выключен просмотр экрана".into(),
        Stop::WindowGone => "окно закрыто".into(),
        Stop::PausedTooLong => "запись всё время стояла на паузе".into(),
        Stop::Broke(why) => why.trim().trim_end_matches('.').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_region_is_clipped_to_the_monitors_and_made_even() {
        let desktop = desktop_of(&[rect(0, 0, 1920, 1080), rect(-1280, 56, 1280, 1024)]);
        assert_eq!(desktop, rect(-1280, 0, 3200, 1080));
        assert_eq!(
            even_region(rect(0, 0, 1920, 1080), desktop),
            Some(rect(0, 0, 1920, 1080))
        );
        // Odd sides lose a pixel.
        assert_eq!(
            even_region(rect(101, 51, 801, 601), desktop),
            Some(rect(101, 51, 800, 600))
        );
        // A window that hangs over the edge is recorded where it can be seen.
        assert_eq!(
            even_region(rect(1500, 900, 800, 600), desktop),
            Some(rect(1500, 900, 420, 180))
        );
        assert_eq!(
            even_region(rect(-1400, -30, 500, 400), desktop),
            Some(rect(-1280, 0, 380, 370))
        );
        // Too little is left, or nothing at all.
        assert_eq!(even_region(rect(1900, 0, 400, 400), desktop), None);
        assert_eq!(even_region(rect(5000, 5000, 400, 400), desktop), None);
        assert_eq!(even_region(rect(10, 10, 63, 500), desktop), None);
        assert_eq!(
            even_region(rect(10, 10, 64, 65), desktop),
            Some(rect(10, 10, 64, 64))
        );
        assert_eq!(desktop_of(&[]), Rect::default());
    }

    #[test]
    fn the_file_is_named_by_the_time_and_never_takes_a_name() {
        let d = tempfile::tempdir().unwrap();
        let profile = d.path().display().to_string();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 14)
            .unwrap()
            .and_hms_opt(15, 30, 5)
            .unwrap();
        let first = output_path(&profile, now);
        assert_eq!(
            first,
            d.path()
                .join("Videos")
                .join("Mika")
                .join("Запись экрана 2026-11-14 15-30-05.mp4")
        );
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = output_path(&profile, now);
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "Запись экрана 2026-11-14 15-30-05 (2).mp4"
        );

        let work = work_dir(&first);
        assert_eq!(
            work,
            first.with_file_name("Запись экрана 2026-11-14 15-30-05.mika-part")
        );
        std::fs::create_dir(&work).unwrap();
        assert_eq!(
            work_dir(&first).file_name().unwrap().to_string_lossy(),
            "Запись экрана 2026-11-14 15-30-05 (2).mika-part",
            "a taken name is counted up and still ends as Mika's own"
        );
    }

    #[test]
    fn sound_is_laid_against_the_first_frame() {
        // The take began 0.4 s before the first frame: that much is cut.
        assert!((sound_offset(0.9, 0.5) - 0.4).abs() < 1e-9);
        // It began after: the sound is delayed.
        assert!((sound_offset(0.2, 0.5) + 0.3).abs() < 1e-9);
        assert_eq!(sound_offset(1.0, 1.0), 0.0);
    }

    #[test]
    fn a_blind_cut_drops_the_tail_of_a_piece() {
        assert_eq!(keep_seconds(10.0, None), Some(10.0));
        assert_eq!(keep_seconds(10.0, Some(10.0)), Some(9.25));
        assert_eq!(
            keep_seconds(1.0, Some(1.0)),
            None,
            "0.25 s is not worth keeping"
        );
        assert_eq!(keep_seconds(1.05, Some(1.05)), Some(1.05 - TRIM_TAIL_S));
        assert_eq!(keep_seconds(0.2, None), None);
        assert_eq!(keep_seconds(0.0, None), None);
        assert_eq!(keep_seconds(f64::NAN, None), None);
        // FFmpeg wrote ten more seconds before it ended: they show the
        // window and are cut with the tail.
        assert_eq!(keep_seconds(70.0, Some(60.0)), Some(59.25));
        // It reported less in the end than at the notice: the shorter wins.
        assert_eq!(keep_seconds(59.5, Some(60.0)), Some(58.75));
        assert_eq!(keep_seconds(30.0, Some(0.0)), None);
    }

    #[test]
    fn areas_overlap_only_when_they_share_a_pixel() {
        let screen = rect(0, 0, 1920, 1080);
        assert!(overlaps(screen, rect(100, 100, 800, 600)));
        assert!(overlaps(rect(100, 100, 800, 600), screen));
        assert!(overlaps(screen, rect(1900, 1000, 500, 500)));
        // Side by side, on the other monitor, and an empty one.
        assert!(!overlaps(screen, rect(1920, 0, 2560, 1440)));
        assert!(!overlaps(screen, rect(-1280, 0, 1280, 1024)));
        assert!(!overlaps(screen, rect(10, 10, 0, 50)));
    }

    #[test]
    fn a_long_recording_gets_longer_to_be_put_together() {
        assert_eq!(stop_grace(60.0), Duration::from_secs(360));
        // Half an hour: 60 + 4 × (60 + 450).
        assert_eq!(stop_grace(1800.0), Duration::from_secs(2100));
        assert_eq!(stop_grace(7200.0), Duration::from_secs(7500));
        assert!(stop_grace(0.0) >= STOP_GRACE);
    }

    #[test]
    fn the_outcome_is_told_in_words() {
        let path = Path::new("C:\\Users\\me\\Videos\\Mika\\Запись экрана.mp4");
        assert_eq!(
            done_text(path, 51_066_000, 135.0, 1800.0, 0, &Stop::User, None),
            "Запись сохранена: C:\\Users\\me\\Videos\\Mika\\Запись экрана.mp4, 48,7 МБ, 2:15."
        );
        let text = done_text(
            path,
            51_066_000,
            1795.0,
            1800.0,
            2,
            &Stop::Limit,
            Some("звук компьютера записать не удалось: нет устройства вывода звука"),
        );
        assert_eq!(
            text,
            "Запись сохранена: C:\\Users\\me\\Videos\\Mika\\Запись экрана.mp4, 48,7 МБ, 29:55. \
Запись 2 раза вставала на паузу из-за окна из слепой зоны: эти места в файл не попали. \
Остановлена: достигнут предел 30 минут. \
Звук компьютера записать не удалось: нет устройства вывода звука."
        );
        let tail = |stop: Stop| {
            let text = done_text(path, 1, 1.0, 60.0, 0, &stop, None);
            text.split_once("0:01. ").unwrap().1.to_string()
        };
        assert_eq!(tail(Stop::Limit), "Остановлена: достигнут предел 1 минуты.");
        assert_eq!(tail(Stop::Disk), "Остановлена: на диске мало места.");
        assert_eq!(
            tail(Stop::ScreenOff),
            "Остановлена: выключен просмотр экрана."
        );
        assert_eq!(tail(Stop::WindowGone), "Остановлена: окно закрыто.");
        assert_eq!(tail(Stop::PausedTooLong), "Остановлена: пауза затянулась.");
        assert_eq!(
            tail(Stop::Broke("захват экрана прервался".into())),
            "Запись оборвалась: захват экрана прервался."
        );
        let once = done_text(path, 1, 1.0, 60.0, 1, &Stop::User, None);
        assert!(once.contains("Запись 1 раз вставала на паузу"), "{once}");
        let many = done_text(path, 1, 1.0, 60.0, 5, &Stop::User, None);
        assert!(many.contains("Запись 5 раз вставала на паузу"), "{many}");
        assert_eq!(minutes_text(30.0), "30 минут");
        assert_eq!(minutes_text(21.0), "21 минуты");
        assert_eq!(minutes_text(11.0), "11 минут");
        assert_eq!(minutes_text(1.5), "1,5 минуты");
        assert_eq!(
            nothing_text(&Stop::Broke("нет экрана.".into())),
            "нет экрана"
        );
    }
}
