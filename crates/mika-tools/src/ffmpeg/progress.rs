//! FFmpeg's `-progress pipe:1` lines as a share of the job.

/// The share of the whole job after one `key=value` line, or `None` when the
/// line tells nothing new. `window` is the part of the job this pass covers:
/// (0, 50) and (50, 100) for two passes.
pub fn share(line: &str, duration_s: f64, window: (u8, u8)) -> Option<u8> {
    let (key, value) = line.trim().split_once('=')?;
    let (from, to) = (window.0.min(100), window.1.clamp(window.0.min(100), 100));
    let done = match key.trim() {
        // Both are microseconds: `out_time_ms` is misnamed.
        "out_time_us" | "out_time_ms" => {
            let us: f64 = value.trim().parse().ok()?;
            if !us.is_finite() || us < 0.0 || duration_s <= 0.0 {
                return None;
            }
            (us / (duration_s * 1e6)).min(1.0)
        }
        "progress" if value.trim() == "end" => 1.0,
        _ => return None,
    };
    Some(from + ((to - from) as f64 * done).floor() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microseconds_become_a_share_of_the_window() {
        assert_eq!(share("out_time_us=5000000", 10.0, (0, 100)), Some(50));
        assert_eq!(share("out_time_ms=5000000", 10.0, (0, 100)), Some(50));
        assert_eq!(share("out_time_us=5000000", 10.0, (50, 100)), Some(75));
        assert_eq!(share("out_time_us=99000000", 10.0, (0, 40)), Some(40));
        assert_eq!(share("progress=end", 10.0, (10, 60)), Some(60));
        assert_eq!(share("progress=continue", 10.0, (0, 100)), None);
        assert_eq!(share("out_time_us=N/A", 10.0, (0, 100)), None);
        assert_eq!(share("out_time_us=-12", 10.0, (0, 100)), None);
        assert_eq!(share("out_time=00:00:05.000000", 10.0, (0, 100)), None);
        assert_eq!(share("frame=12", 10.0, (0, 100)), None);
        assert_eq!(share("out_time_us=1", 0.0, (0, 100)), None);
    }
}
