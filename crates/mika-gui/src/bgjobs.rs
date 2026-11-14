//! Background jobs for the page: the list of jobs that still run and the stop
//! button of a job card. The jobs themselves live in `mika_core::bgjobs`.

use std::sync::Arc;

use mika_core::bgjobs::{JobBoard, JobId, JobView};

use crate::app::Ctx;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// Jobs that wait, work or are being stopped. The page draws a card for each
/// when its chat is drawn anew.
#[tauri::command]
pub fn bg_jobs_list(ctx: CtxState<'_>) -> Vec<JobView> {
    ctx.jobs.active()
}

/// The stop button of a job card. A waiting job leaves the queue, a working one
/// is told to stop, a recording is saved. The card follows `bg_job_updated`.
#[tauri::command]
pub fn bg_job_stop(ctx: CtxState<'_>, id: JobId) -> Result<(), String> {
    stop(&ctx.jobs, id)
}

fn stop(jobs: &JobBoard, id: JobId) -> Result<(), String> {
    jobs.stop(id)
        .map(|_| ())
        .map_err(|_| tr("error.bgjob_gone"))
}

/// The recording that runs now, if any: the tray item «stop recording» acts on it.
pub fn recording(jobs: &JobBoard) -> Option<JobView> {
    jobs.active().into_iter().find(|j| j.live)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mika_core::bgjobs::{JobCtx, JobSpec, JobState, JobWork};
    use mika_core::tools::ToolOutput;
    use mika_core::EventBus;

    use super::*;

    /// Work that ends only when it is told to stop, and ends well: the way a recording does.
    fn until_stopped() -> JobWork {
        Box::new(|ctx: JobCtx| {
            Box::pin(async move {
                ctx.cancel.cancelled().await;
                Ok(ToolOutput::new("saved"))
            })
        })
    }

    #[tokio::test]
    async fn stop_reaches_the_job_and_a_finished_one_is_gone() {
        let jobs = JobBoard::new(EventBus::new(64));
        let id = jobs
            .submit(JobSpec::queued("video_compress", "Сжать"), until_stopped())
            .unwrap()
            .job
            .id;
        assert_eq!(stop(&jobs, id), Ok(()));
        let ended = jobs.wait(id, Duration::from_secs(5)).await.unwrap();
        assert!(!ended.state.active());
        // The card may still show the button for a moment: the answer is a text, not a crash.
        assert_eq!(stop(&jobs, id), Err(tr("error.bgjob_gone")));
        assert_eq!(stop(&jobs, id + 100), Err(tr("error.bgjob_gone")));
        assert_ne!(tr("error.bgjob_gone"), "error.bgjob_gone");
    }

    #[tokio::test]
    async fn recording_is_the_live_job_and_only_while_it_runs() {
        let jobs = JobBoard::new(EventBus::new(64));
        assert!(recording(&jobs).is_none());
        jobs.submit(JobSpec::queued("video_compress", "Сжать"), until_stopped())
            .unwrap();
        assert!(recording(&jobs).is_none(), "a queue job is not a recording");
        let rec = jobs
            .submit(
                JobSpec::live("screen_record", "Записать экран", Duration::from_secs(5)),
                until_stopped(),
            )
            .unwrap()
            .job;
        assert_eq!(recording(&jobs).map(|j| j.id), Some(rec.id));
        stop(&jobs, rec.id).unwrap();
        let ended = jobs.wait(rec.id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(ended.state, JobState::Done);
        assert!(recording(&jobs).is_none());
        jobs.shutdown(Duration::from_secs(5)).await;
    }
}
