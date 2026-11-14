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

/// The last line FFmpeg complained with.
fn last_complaint(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
}

/// The record of one FFmpeg run: how long it took and how it ended, with the
/// exit code and the last line of its complaint when it failed. The
/// arguments name the user's files and are never written, and of the
/// complaint, which quotes them, only the reason is kept while texts are off
/// (`journal::complaint`). `what` names the work and whose it is: the tool,
/// then the job or the turn. `stopped` says the run was ended on purpose.
pub(crate) fn run_line(
    what: &str,
    ms: u128,
    out: &Result<RunOutput, String>,
    stopped: bool,
) -> (log::Level, String) {
    let head = format!("ffmpeg run finished: what={what} ms={ms}");
    let code = |o: &RunOutput| o.code.map_or("none".to_string(), |c| c.to_string());
    match out {
        Err(e) if stopped || e == "остановлено" => {
            (log::Level::Info, format!("{head} outcome=stopped"))
        }
        Err(e) => (
            log::Level::Warn,
            format!(
                "{head} outcome=not_run error={}",
                mika_core::journal::error_text(e)
            ),
        ),
        Ok(o) if stopped => (
            log::Level::Info,
            format!("{head} code={} outcome=stopped", code(o)),
        ),
        Ok(o) if o.timed_out => (
            log::Level::Warn,
            format!("{head} code={} outcome=timed_out", code(o)),
        ),
        Ok(o) if o.code == Some(0) => (log::Level::Info, format!("{head} code=0 outcome=ok")),
        Ok(o) => (
            log::Level::Warn,
            format!(
                "{head} code={} outcome=error complaint={}",
                code(o),
                mika_core::journal::complaint(last_complaint(&o.stderr))
            ),
        ),
    }
}

/// Whose run it is, for its record: the background job it works in, or the
/// turn whose tool started it. A run with neither is somebody else's, such
/// as a voice message being converted.
fn owner() -> String {
    if let Some(job) = mika_core::bgjobs::job_handle() {
        return format!(" job={}", job.id());
    }
    mika_core::journal::turn().map_or_else(String::new, |turn| format!(" turn={turn}"))
}

/// Runs FFmpeg with `args`. Its progress fills `window` of the whole job.
/// `what` names the work in the record of the run: `av_convert`,
/// `record_mux`, `voice`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ffmpeg(
    what: &str,
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
    let began = std::time::Instant::now();
    let out = runner
        .run_streaming(
            &paths.ffmpeg.to_string_lossy(),
            args,
            cwd,
            timeout,
            cancel.clone(),
            &on_line,
        )
        .await;
    let (level, line) = run_line(
        &format!("{what}{} ffmpeg={}", owner(), paths.source.as_str()),
        began.elapsed().as_millis(),
        &out,
        cancel.is_cancelled(),
    );
    log::log!(target: "jobs", level, "{line}");
    let out = out.map_err(|e| {
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
    fn the_record_of_a_run_has_the_code_and_the_last_complaint_only() {
        let failed = Ok(RunOutput {
            code: Some(1),
            stderr: "Input #0, mov\n  Stream #0:0\nD:/v/Отпуск.mp4: Invalid data found when processing input\n\n".into(),
            ..Default::default()
        });
        // Texts are off in tests: the reason is kept, the path is not.
        assert_eq!(
            run_line("av_convert job=7 ffmpeg=path", 12, &failed, false),
            (
                log::Level::Warn,
                "ffmpeg run finished: what=av_convert job=7 ffmpeg=path ms=12 code=1 outcome=error complaint=<hidden 15 chars>: Invalid data found when processing input"
                    .to_string()
            )
        );
        // A last line that quotes a name is not kept at all.
        let quoting = Ok(RunOutput {
            code: Some(1),
            stderr: "Unable to find a suitable output format for 'D:/v/a.xyz'\n".into(),
            ..Default::default()
        });
        let (_, line) = run_line("job", 1, &quoting, false);
        assert!(
            line.ends_with("outcome=error complaint=<hidden 56 chars>"),
            "{line}"
        );
        let good = Ok(RunOutput {
            code: Some(0),
            stderr: "frame=  10\n".into(),
            ..Default::default()
        });
        assert_eq!(
            run_line("job", 5, &good, false),
            (
                log::Level::Info,
                "ffmpeg run finished: what=job ms=5 code=0 outcome=ok".to_string()
            )
        );
        // A run ended on purpose is no failure, whatever FFmpeg returned.
        assert_eq!(
            run_line("job", 5, &failed, true),
            (
                log::Level::Info,
                "ffmpeg run finished: what=job ms=5 code=1 outcome=stopped".to_string()
            )
        );
        assert_eq!(
            run_line("job", 5, &Err("остановлено".into()), false).0,
            log::Level::Info
        );
        let timed = Ok(RunOutput {
            timed_out: true,
            ..Default::default()
        });
        assert_eq!(
            run_line("capture", 9, &timed, false),
            (
                log::Level::Warn,
                "ffmpeg run finished: what=capture ms=9 code=none outcome=timed_out".to_string()
            )
        );
        let (level, line) = run_line("job", 1, &Err("нет файла ffmpeg.exe".into()), false);
        assert_eq!(level, log::Level::Warn);
        assert_eq!(
            line,
            "ffmpeg run finished: what=job ms=1 outcome=not_run error=нет файла ffmpeg.exe"
        );
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
                    "job",
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
