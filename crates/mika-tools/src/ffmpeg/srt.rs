//! SubRip subtitles: at most two lines of 42 characters per cue.

const LINE: usize = 42;
const LINES: usize = 2;

/// One phrase with its time, seconds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// `HH:MM:SS,mmm`.
pub(crate) fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Words into lines of at most `LINE` characters; a longer word is cut.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let len = word.chars().count();
            let have = cur.chars().count();
            if have == 0 && len <= LINE {
                cur = word;
                break;
            }
            if have > 0 && have + 1 + len <= LINE {
                cur.push(' ');
                cur.push_str(&word);
                break;
            }
            if have > 0 {
                lines.push(std::mem::take(&mut cur));
                continue;
            }
            let head: String = word.chars().take(LINE).collect();
            word = word.chars().skip(LINE).collect();
            lines.push(head);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Cues that fit on the screen: a long phrase becomes several, each with a
/// part of its time by the share of its characters.
pub(crate) fn fit(cues: &[Cue]) -> Vec<Cue> {
    let mut out = Vec::new();
    for cue in cues {
        let lines = wrap(&cue.text);
        if lines.is_empty() {
            continue;
        }
        let groups: Vec<String> = lines.chunks(LINES).map(|g| g.join("\n")).collect();
        let total: usize = groups
            .iter()
            .map(|g| g.chars().count())
            .sum::<usize>()
            .max(1);
        let span = (cue.end - cue.start).max(0.0);
        let mut start = cue.start;
        for (i, g) in groups.iter().enumerate() {
            let end = if i + 1 == groups.len() {
                cue.end
            } else {
                start + span * g.chars().count() as f64 / total as f64
            };
            out.push(Cue {
                start,
                end,
                text: g.clone(),
            });
            start = end;
        }
    }
    out
}

/// The whole file: CRLF line ends, UTF-8 without BOM.
pub(crate) fn render(cues: &[Cue]) -> String {
    let mut s = String::new();
    for (i, c) in fit(cues).iter().enumerate() {
        s.push_str(&format!(
            "{}\r\n{} --> {}\r\n{}\r\n\r\n",
            i + 1,
            srt_time(c.start),
            srt_time(c.end),
            c.text.replace('\n', "\r\n")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_hours_minutes_seconds_and_milliseconds() {
        assert_eq!(srt_time(0.0), "00:00:00,000");
        assert_eq!(srt_time(3723.4567), "01:02:03,457");
    }

    #[test]
    fn a_short_phrase_is_one_cue_with_crlf() {
        let s = render(&[Cue {
            start: 1.0,
            end: 2.5,
            text: " Привет, мир. ".into(),
        }]);
        assert_eq!(
            s,
            "1\r\n00:00:01,000 --> 00:00:02,500\r\nПривет, мир.\r\n\r\n"
        );
    }

    #[test]
    fn a_long_phrase_splits_by_the_share_of_its_characters() {
        let text = "слово ".repeat(30);
        let cues = fit(&[Cue {
            start: 0.0,
            end: 10.0,
            text,
        }]);
        assert!(cues.len() >= 2, "{cues:?}");
        for c in &cues {
            let lines: Vec<&str> = c.text.split('\n').collect();
            assert!(lines.len() <= 2);
            assert!(lines.iter().all(|l| l.chars().count() <= 42), "{c:?}");
        }
        assert_eq!(cues[0].start, 0.0);
        assert_eq!(cues.last().unwrap().end, 10.0);
        for w in cues.windows(2) {
            assert_eq!(w[0].end, w[1].start);
            assert!(w[0].end > w[0].start);
        }
        let long_word = fit(&[Cue {
            start: 0.0,
            end: 1.0,
            text: "а".repeat(100),
        }]);
        assert!(long_word
            .iter()
            .all(|c| c.text.split('\n').all(|l| l.chars().count() <= 42)));
        assert!(fit(&[Cue {
            start: 0.0,
            end: 1.0,
            text: "  ".into()
        }])
        .is_empty());
    }
}
