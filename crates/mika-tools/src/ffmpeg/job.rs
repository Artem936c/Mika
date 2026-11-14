//! One FFmpeg run with its progress, time limit and errors in words.

use std::path::Path;
use std::time::Duration;

use mika_core::tools::{Progress, ToolError};
use tokio_util::sync::CancellationToken;

use super::FfmpegPaths;
use crate::sys::{RunOutput, Runner};

/// Time for a job that encodes: two minutes plus eight times the length,
/// at most eight hours.
pub(crate) fn encode_timeout(duration_s: f64) -> Duration {
    Duration::from_secs_f64((120.0 + 8.0 * duration_s.max(0.0)).min(8.0 * 3600.0))
}

/// Time for a job that only copies streams: a minute plus a quarter of the
/// length, at most an hour.
pub(crate) fn copy_timeout(duration_s: f64) -> Duration {
    Duration::from_secs_f64((60.0 + duration_s.max(0.0) / 4.0).min(3600.0))
}

/// Time for one frame.
pub(crate) const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// The last lines FFmpeg complained with, at most 500 characters.
fn complaint(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(5)..].join("; ");
    let count = tail.chars().count();
    if count > 500 {
        tail.chars().skip(count - 500).collect()
    } else {
        tail
    }
}

/// Runs FFmpeg with `args`. Its progress fills `window` of the whole job.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ffmpeg(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    args: &[String],
    cwd: Option<&Path>,
    duration_s: f64,
    window: (u8, u8),
    progress: &Progress,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<RunOutput, ToolError> {
    let on_line = |line: &str| {
        if let Some(share) = super::progress::share(line, duration_s, window) {
            progress.set(share);
        }
    };
    let out = runner
        .run_streaming(
            &paths.ffmpeg.to_string_lossy(),
            args,
            cwd,
            timeout,
            cancel.clone(),
            &on_line,
        )
        .await
        .map_err(|e| {
            if e == "остановлено" {
                ToolError::Cancelled
            } else {
                ToolError::Failed(e)
            }
        })?;
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    if out.timed_out {
        return Err(ToolError::Failed("FFmpeg не уложился в срок".into()));
    }
    if out.code != Some(0) {
        return Err(ToolError::Failed(format!(
            "FFmpeg завершился с ошибкой: {}",
            complaint(&out.stderr)
        )));
    }
    progress.set(window.1);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_limits_grow_with_length_and_stop() {
        assert_eq!(encode_timeout(10.0), Duration::from_secs(200));
        assert_eq!(encode_timeout(10.0 * 3600.0), Duration::from_secs(8 * 3600));
        assert_eq!(copy_timeout(400.0), Duration::from_secs(160));
        assert_eq!(copy_timeout(1e6), Duration::from_secs(3600));
    }

    #[test]
    fn complaint_keeps_the_last_five_lines() {
        let err = "a\nb\n\nc\nd\ne\nf\n";
        assert_eq!(complaint(err), "b; c; d; e; f");
        assert_eq!(complaint(&"я".repeat(600)).chars().count(), 500);
    }

    #[tokio::test]
    async fn errors_become_words() {
        let (env, f) = crate::testing::env();
        let paths = FfmpegPaths {
            ffmpeg: "ffmpeg.exe".into(),
            ffprobe: "ffprobe.exe".into(),
            source: super::super::FfmpegSource::Path,
        };
        let run = |out: RunOutput| {
            f.run_outputs.lock().push_back(out);
            let runner = env.runner.clone();
            let paths = paths.clone();
            async move {
                run_ffmpeg(
                    runner.as_ref(),
                    &paths,
                    &[],
                    None,
                    10.0,
                    (0, 100),
                    &Progress::default(),
                    &CancellationToken::new(),
                    Duration::from_secs(1),
                )
                .await
            }
        };
        let r = run(RunOutput {
            code: Some(1),
            stderr: "Unknown encoder 'x'\n".into(),
            ..Default::default()
        })
        .await;
        assert_eq!(
            r,
            Err(ToolError::Failed(
                "FFmpeg завершился с ошибкой: Unknown encoder 'x'".into()
            ))
        );
        let r = run(RunOutput {
            timed_out: true,
            ..Default::default()
        })
        .await;
        assert_eq!(
            r,
            Err(ToolError::Failed("FFmpeg не уложился в срок".into()))
        );
        assert!(run(RunOutput {
            code: Some(0),
            ..Default::default()
        })
        .await
        .is_ok());
    }
}
