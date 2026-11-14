//! Times in the arguments, in file names and in answers.

/// `90`, `90.5`, `1:30`, `01:02:03`, `1:02:03,5` as seconds.
pub fn parse_time(raw: &str) -> Result<f64, String> {
    let bad =
        || format!("время «{raw}» не понятно: нужно в секундах (90), м:сс (1:30) или ч:мм:сс");
    let s = raw.trim().replace(',', ".");
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return Err(bad());
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(bad());
    }
    let (last, whole) = parts.split_last().ok_or_else(bad)?;
    let seconds: f64 = last.trim().parse().map_err(|_| bad())?;
    if !seconds.is_finite() || seconds < 0.0 || (!whole.is_empty() && seconds >= 60.0) {
        return Err(bad());
    }
    // Whole minutes: hours and minutes, or minutes alone.
    let mut minutes = 0.0;
    for (i, p) in whole.iter().enumerate() {
        let n: u64 = p.trim().parse().map_err(|_| bad())?;
        // Minutes after hours stay under 60.
        if i > 0 && n >= 60 {
            return Err(bad());
        }
        minutes = minutes * 60.0 + n as f64;
    }
    Ok(minutes * 60.0 + seconds)
}

fn split(t: f64) -> (u64, u64, u64) {
    let s = t.max(0.0).floor() as u64;
    (s / 3600, s / 60 % 60, s % 60)
}

/// For a file name: `01.30` or `1.02.03`.
pub fn name_time(t: f64) -> String {
    match split(t) {
        (0, m, s) => format!("{m:02}.{s:02}"),
        (h, m, s) => format!("{h}.{m:02}.{s:02}"),
    }
}

/// For people: `1:30` or `1:02:03`.
pub fn show_time(t: f64) -> String {
    match split(t) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

/// For FFmpeg: seconds with milliseconds.
pub fn arg_time(t: f64) -> String {
    format!("{:.3}", t.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_in_every_written_form() {
        assert_eq!(parse_time("90"), Ok(90.0));
        assert_eq!(parse_time(" 90.5 "), Ok(90.5));
        assert_eq!(parse_time("1:30"), Ok(90.0));
        assert_eq!(parse_time("01:02:03"), Ok(3723.0));
        assert_eq!(parse_time("1:02:03,5"), Ok(3723.5));
        assert_eq!(parse_time("0:05"), Ok(5.0));
        for bad in [
            "-5", "", "1:", "a", "1:2:3:4", "1:75", "1:75:00", "nan", "inf", "+3",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_are_written_for_names_people_and_ffmpeg() {
        assert_eq!(name_time(90.0), "01.30");
        assert_eq!(name_time(3723.9), "1.02.03");
        assert_eq!(show_time(90.0), "1:30");
        assert_eq!(show_time(751.0), "12:31");
        assert_eq!(show_time(3723.0), "1:02:03");
        assert_eq!(arg_time(1.5), "1.500");
    }
}
