//! Background jobs: long work that goes on after the turn that started it
//! has ended (decision 0032).
//!
//! A job is made only inside `Tool::run`, so after the gate approved the
//! call; the approval travels into the job. The board knows nothing about
//! what the work is: it keeps the queue, the states and the stop.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::events::{Event, EventBus};
use crate::gate::Approval;
use crate::llm::ToolSpec;
use crate::tools::{
    spec, with_progress_handle, Assessment, Progress, RiskLevel, Tool, ToolError, ToolOutput,
};

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Stopping,
    Done,
    Failed,
    Stopped,
}

impl JobState {
    /// Queued, Running or Stopping.
    pub fn active(self) -> bool {
        matches!(
            self,
            JobState::Queued | JobState::Running | JobState::Stopping
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Stopping => "stopping",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Stopped => "stopped",
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobId,
    /// Name of the tool that started it: "video_compress", "av_folder", "screen_record".
    pub kind: String,
    /// What it does, in the words of the confirm card (the assessment summary).
    pub title: String,
    pub state: JobState,
    /// Runs beside the queue and has no known amount of work: a recording.
    pub live: bool,
    /// 0..=100. For a live job: time recorded out of the limit.
    pub share: u8,
    /// A short line under the title: "3 из 12: клип.mp4", "2:15 из 30:00".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The outcome in words once it ended; for a stopped job, what was done before the stop.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Result card once it ended (today always `av_files`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// The caller reports the end itself: the app makes no line of its own about it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub silent: bool,
    /// Started in a turn that came from Telegram (`JobBoard::follow_origin`):
    /// its end is told there, whatever turn runs by then. The end of a job
    /// started at this computer never goes to Telegram. Not for the
    /// interface: it stays out of the JSON.
    #[serde(skip)]
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum JobError {
    #[error("очередь фоновых заданий полна: дождись конца или останови лишние")]
    QueueFull,
    #[error("задания №{0} нет")]
    NotFound(JobId),
    #[error("задание №{0} уже закончено")]
    Finished(JobId),
    #[error("приложение закрывается")]
    Closed,
}

/// Waiting jobs, the running one not counted.
pub const MAX_QUEUED: usize = 10;
/// Finished jobs kept for `bg_jobs`.
pub const KEPT_FINISHED: usize = 20;
/// First words of the text a tool returns when its work went to the background.
pub const STARTED_PREFIX: &str = "Запущено в фоне:";
/// Text of a job stopped before it made anything.
pub const STOPPED_TEXT: &str = "Остановлено.";
/// A job that ends at once still counts as running this long after it was
/// taken. The call that started it shows its card only when its `run` has
/// returned; the final state must come after that card, or the card would
/// show a running job for ever.
const CARD_LEAD: Duration = Duration::from_millis(200);

pub struct JobSpec {
    pub kind: String,
    pub title: String,
    pub live: bool,
    /// Longest wait for the work after a stop before its future is dropped.
    pub grace: Duration,
}

impl JobSpec {
    /// A job that waits for its turn; five seconds to end after a stop.
    pub fn queued(kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            title: title.into(),
            live: false,
            grace: Duration::from_secs(5),
        }
    }

    /// A job that starts at once beside the queue.
    pub fn live(kind: impl Into<String>, title: impl Into<String>, grace: Duration) -> Self {
        Self {
            kind: kind.into(),
            title: title.into(),
            live: true,
            grace,
        }
    }
}

/// What the work of a job gets.
pub struct JobCtx {
    /// Fires when the job is asked to stop. Its own token: «Стоп» of the
    /// chat does not touch it.
    pub cancel: CancellationToken,
    pub handle: JobHandle,
}

pub type JobFuture = Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send>>;
pub type JobWork = Box<dyn FnOnce(JobCtx) -> JobFuture + Send>;

/// The running job as its work sees it.
#[derive(Clone)]
pub struct JobHandle {
    id: JobId,
    board: JobBoard,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Publishes when the share changed.
    pub fn set_share(&self, share: u8) {
        let share = share.min(100);
        self.board.change(self.id, |view| {
            let changed = view.share != share;
            view.share = share;
            changed
        });
    }

    /// Publishes when the note changed.
    pub fn set_note(&self, note: &str) {
        self.board.change(self.id, |view| {
            let changed = view.note != note;
            if changed {
                view.note = note.to_string();
            }
            changed
        });
    }

    /// What is done so far. Shown as `text`/`data` if the job is then stopped
    /// or fails. Publishes nothing.
    pub fn set_partial(&self, text: &str, data: Option<Value>) {
        let mut b = self.board.inner.board.lock();
        if let Some(job) = b.jobs.get_mut(&self.id) {
            if job.view.state.active() {
                job.partial = Some((text.to_string(), data));
            }
        }
    }

    /// Progress that feeds `set_share`, at most every 250 ms.
    pub fn progress(&self) -> Progress {
        let handle = self.clone();
        Progress::to(move |share| handle.set_share(share))
    }
}

tokio::task_local! {
    static CURRENT: JobHandle;
}

/// The job this task runs in, if any.
pub fn job_handle() -> Option<JobHandle> {
    CURRENT.try_with(Clone::clone).ok()
}

/// A job as `submit` left it.
pub struct Submitted {
    pub job: JobView,
    /// Queue jobs that will run before it.
    pub ahead: usize,
}

struct Job {
    view: JobView,
    cancel: CancellationToken,
    grace: Duration,
    partial: Option<(String, Option<Value>)>,
    /// The work of a job that still waits.
    work: Option<JobWork>,
    /// When the work was taken.
    taken: Instant,
}

#[derive(Default)]
struct Board {
    last_id: JobId,
    jobs: BTreeMap<JobId, Job>,
    /// Waiting jobs, the next one first.
    queue: VecDeque<JobId>,
    /// Ended jobs, the oldest first.
    finished: VecDeque<JobId>,
    /// The task that runs the queue is alive.
    worker: bool,
    closed: bool,
}

impl Board {
    /// The job `id` has ended: it joins the kept ones, the oldest leave.
    fn keep_finished(&mut self, id: JobId) {
        self.finished.push_back(id);
        while self.finished.len() > KEPT_FINISHED {
            if let Some(old) = self.finished.pop_front() {
                self.jobs.remove(&old);
            }
        }
    }

    /// A queue job is running or stopping.
    fn queue_busy(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| {
                !j.view.live && matches!(j.view.state, JobState::Running | JobState::Stopping)
            })
            .count()
    }
}

/// Says whether the turn that runs now came from Telegram.
type RemoteFn = Arc<dyn Fn() -> bool + Send + Sync>;

struct Inner {
    bus: EventBus,
    board: Mutex<Board>,
    /// Asked when a job is made; `None`: every job is of this computer.
    remote: Mutex<Option<RemoteFn>>,
    /// Counts ended jobs: `wait` and `shutdown` wake on a change.
    ended: tokio::sync::watch::Sender<u64>,
}

/// The queue of background jobs. Cloning gives another handle to the same board.
#[derive(Clone)]
pub struct JobBoard {
    inner: Arc<Inner>,
}

type Outcome = Result<Result<ToolOutput, ToolError>, Box<dyn std::any::Any + Send>>;

impl JobBoard {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Inner {
                bus,
                board: Mutex::new(Board::default()),
                remote: Mutex::new(None),
                ended: tokio::sync::watch::channel(0).0,
            }),
        }
    }

    /// Sets who says whether the turn that runs now came from Telegram. A
    /// job is made inside `Tool::run`, so in the turn that asked for it and
    /// after its question was answered: the answer of that moment is kept in
    /// the job (`JobView::remote`), and the end of the job is told by it, not
    /// by the turn that happens to run when the job ends.
    pub fn follow_origin(&self, remote: impl Fn() -> bool + Send + Sync + 'static) {
        *self.inner.remote.lock() = Some(Arc::new(remote));
    }

    /// Tells everyone the state of a job. Called with the board locked, so
    /// events come in the order of the changes.
    fn publish(&self, view: &JobView) {
        self.inner
            .bus
            .publish(Event::BgJobUpdated { job: view.clone() });
    }

    /// Changes a job that still works; publishes when `f` says it changed.
    fn change(&self, id: JobId, f: impl FnOnce(&mut JobView) -> bool) {
        let mut b = self.inner.board.lock();
        let Some(job) = b.jobs.get_mut(&id) else {
            return;
        };
        if !matches!(job.view.state, JobState::Running | JobState::Stopping) {
            return;
        }
        if f(&mut job.view) {
            self.publish(&job.view);
        }
    }

    /// Needs a tokio runtime. A queue job waits for its turn; a live job starts at once.
    pub fn submit(&self, spec: JobSpec, work: JobWork) -> Result<Submitted, JobError> {
        // Asked before the board is locked: the answer comes from the session.
        let asks = self.inner.remote.lock().clone();
        let remote = asks.is_some_and(|remote| remote());
        let (view, ahead, start) = {
            let mut b = self.inner.board.lock();
            if b.closed {
                return Err(JobError::Closed);
            }
            let waits = !spec.live && b.worker;
            if waits && b.queue.len() >= MAX_QUEUED {
                log::warn!("job refused: the queue is full: kind={}", spec.kind);
                return Err(JobError::QueueFull);
            }
            b.last_id += 1;
            let id = b.last_id;
            let ahead = if waits {
                b.queue.len() + b.queue_busy()
            } else {
                0
            };
            let mut job = Job {
                view: JobView {
                    id,
                    kind: spec.kind,
                    title: spec.title,
                    state: if waits {
                        JobState::Queued
                    } else {
                        JobState::Running
                    },
                    live: spec.live,
                    share: 0,
                    note: String::new(),
                    text: String::new(),
                    data: None,
                    silent: false,
                    remote,
                },
                cancel: CancellationToken::new(),
                grace: spec.grace,
                partial: None,
                work: None,
                taken: Instant::now(),
            };
            let start = if waits {
                job.work = Some(work);
                b.queue.push_back(id);
                None
            } else {
                if !spec.live {
                    b.worker = true;
                }
                Some(work)
            };
            let view = job.view.clone();
            b.jobs.insert(id, job);
            self.publish(&view);
            (view, ahead, start)
        };
        // The title is the summary of the call and may name a file: the
        // record carries the tool and the number only.
        if start.is_some() {
            log::info!(
                "job started: id={} kind={} live={}",
                view.id,
                view.kind,
                view.live
            );
        } else {
            log::info!(
                "job queued: id={} kind={} ahead={ahead}",
                view.id,
                view.kind
            );
        }
        if let Some(work) = start {
            let board = self.clone();
            let id = view.id;
            if view.live {
                tokio::spawn(async move { board.run_job(id, work).await });
            } else {
                tokio::spawn(async move { board.run_queue(id, work).await });
            }
        }
        Ok(Submitted { job: view, ahead })
    }

    /// Runs queue jobs one after another until none waits.
    async fn run_queue(self, first: JobId, work: JobWork) {
        let mut next = Some((first, work));
        while let Some((id, work)) = next.take() {
            self.run_job(id, work).await;
            let mut b = self.inner.board.lock();
            while let Some(id) = b.queue.pop_front() {
                let Some(job) = b.jobs.get_mut(&id) else {
                    continue;
                };
                let Some(work) = job.work.take() else {
                    continue;
                };
                job.view.state = JobState::Running;
                log::info!(
                    "job started: id={id} kind={} live=false waited_ms={}",
                    job.view.kind,
                    job.taken.elapsed().as_millis()
                );
                job.taken = Instant::now();
                self.publish(&job.view);
                next = Some((id, work));
                break;
            }
            if next.is_none() {
                b.worker = false;
            }
        }
    }

    /// Runs the work of one job to its end and records the outcome.
    async fn run_job(&self, id: JobId, work: JobWork) {
        let (cancel, grace, taken) = {
            let b = self.inner.board.lock();
            match b.jobs.get(&id) {
                Some(job) => (job.cancel.clone(), job.grace, job.taken),
                None => return,
            }
        };
        let handle = JobHandle {
            id,
            board: self.clone(),
        };
        let ctx = JobCtx {
            cancel: cancel.clone(),
            handle: handle.clone(),
        };
        let progress = handle.progress();
        let scoped = CURRENT.scope(
            handle,
            with_progress_handle(progress, async move { work(ctx).await }),
        );
        let mut fut = Box::pin(AssertUnwindSafe(scoped).catch_unwind());
        let mut dropped = false;
        let outcome: Outcome = tokio::select! {
            r = &mut fut => r,
            _ = cancel.cancelled() => match tokio::time::timeout(grace, &mut fut).await {
                Ok(r) => r,
                // The work did not end in time: dropping it kills its
                // programs and removes its half-written files. For a
                // recording this is "the file was not saved", so it is told
                // apart from a stop that ended well.
                Err(_) => {
                    dropped = true;
                    log::warn!(
                        "job did not end within its grace after the stop, its work is dropped: id={id} grace_ms={}",
                        grace.as_millis()
                    );
                    Ok(Err(ToolError::Cancelled))
                }
            },
        };
        drop(fut);
        if let Some(rest) = CARD_LEAD.checked_sub(taken.elapsed()) {
            tokio::time::sleep(rest).await;
        }
        self.finish(id, outcome, dropped);
    }

    /// `dropped`: the work was thrown away because it did not end in time
    /// after the stop.
    fn finish(&self, id: JobId, outcome: Outcome, dropped: bool) {
        let mut b = self.inner.board.lock();
        let Some(job) = b.jobs.get_mut(&id) else {
            return;
        };
        let (partial_text, partial_data) = job.partial.take().unwrap_or_default();
        let ms = job.taken.elapsed().as_millis();
        let view = &mut job.view;
        match outcome {
            Ok(Ok(out)) => {
                log::info!(
                    "job finished: id={id} kind={} state=done ms={ms}",
                    view.kind
                );
                view.state = JobState::Done;
                view.share = 100;
                view.text = out.text;
                view.data = out.data;
            }
            Ok(Err(ToolError::Cancelled)) => {
                log::info!(
                    "job finished: id={id} kind={} state=stopped ms={ms} dropped={dropped}",
                    view.kind
                );
                view.state = JobState::Stopped;
                view.text = if partial_text.trim().is_empty() {
                    STOPPED_TEXT.to_string()
                } else {
                    partial_text
                };
                view.data = partial_data;
            }
            Ok(Err(e)) => {
                // The text of the error is the tool's: it may quote a path
                // or a title, so it is written only while texts are.
                let error_kind = match &e {
                    ToolError::BadArguments(_) => "bad_arguments",
                    ToolError::NotReady { .. } => "not_ready",
                    _ => "failed",
                };
                log::error!(
                    "job finished: id={id} kind={} state=failed ms={ms} error_kind={error_kind} error={}",
                    view.kind,
                    crate::journal::guarded_error(&e)
                );
                view.state = JobState::Failed;
                view.text = join_text(&e.to_string(), &partial_text);
                view.data = partial_data;
            }
            Err(_) => {
                log::error!(
                    "job finished: id={id} kind={} state=failed ms={ms} error=the work panicked",
                    view.kind
                );
                view.state = JobState::Failed;
                view.text = join_text("внутренняя ошибка задания", &partial_text);
                view.data = partial_data;
            }
        }
        self.publish(view);
        b.keep_finished(id);
        self.inner.ended.send_modify(|n| *n += 1);
    }

    pub fn get(&self, id: JobId) -> Option<JobView> {
        self.inner
            .board
            .lock()
            .jobs
            .get(&id)
            .map(|j| j.view.clone())
    }

    /// Running first, then queued in order, then finished ones, newest first.
    pub fn list(&self) -> Vec<JobView> {
        let b = self.inner.board.lock();
        let view = |id: &JobId| b.jobs.get(id).map(|j| j.view.clone());
        let mut out: Vec<JobView> = b
            .jobs
            .values()
            .filter(|j| matches!(j.view.state, JobState::Running | JobState::Stopping))
            .map(|j| j.view.clone())
            .collect();
        out.extend(b.queue.iter().filter_map(view));
        out.extend(b.finished.iter().rev().filter_map(view));
        out
    }

    /// Only jobs whose state is active.
    pub fn active(&self) -> Vec<JobView> {
        let mut list = self.list();
        list.retain(|j| j.state.active());
        list
    }

    /// Queued: removed at once, state Stopped. Running: token cancelled,
    /// state Stopping. Stopping: Ok, nothing new.
    pub fn stop(&self, id: JobId) -> Result<JobView, JobError> {
        // The work of a waiting job is dropped after the lock: it may own
        // files and programs.
        let (view, _work) = {
            let mut b = self.inner.board.lock();
            let job = b.jobs.get_mut(&id).ok_or(JobError::NotFound(id))?;
            match job.view.state {
                JobState::Queued => {
                    log::info!(
                        "job finished: id={id} kind={} state=stopped before it started",
                        job.view.kind
                    );
                    job.view.state = JobState::Stopped;
                    job.view.text = STOPPED_TEXT.to_string();
                    let work = job.work.take();
                    let view = job.view.clone();
                    self.publish(&view);
                    b.queue.retain(|q| *q != id);
                    b.keep_finished(id);
                    self.inner.ended.send_modify(|n| *n += 1);
                    (view, work)
                }
                JobState::Running => {
                    log::info!("job asked to stop: id={id} kind={}", job.view.kind);
                    job.view.state = JobState::Stopping;
                    job.cancel.cancel();
                    self.publish(&job.view);
                    (job.view.clone(), None)
                }
                JobState::Stopping => (job.view.clone(), None),
                JobState::Done | JobState::Failed | JobState::Stopped => {
                    return Err(JobError::Finished(id));
                }
            }
        };
        Ok(view)
    }

    /// Whether the app tells about the end of the job by itself. Seen in the
    /// events that follow; publishes nothing.
    pub fn set_silent(&self, id: JobId, silent: bool) {
        if let Some(job) = self.inner.board.lock().jobs.get_mut(&id) {
            job.view.silent = silent;
        }
    }

    /// Waits until the job has ended; `None` after `timeout` and for a job
    /// that is not known.
    pub async fn wait(&self, id: JobId, timeout: Duration) -> Option<JobView> {
        let mut ended = self.inner.ended.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let view = self.get(id)?;
            if !view.state.active() {
                return Some(view);
            }
            match tokio::time::timeout_at(deadline, ended.changed()).await {
                Ok(Ok(())) => {}
                _ => return self.get(id).filter(|v| !v.state.active()),
            }
        }
    }

    /// Stops every job that waits or works (decision 0045): a waiting one
    /// leaves the queue, a working one is told to stop, a recording saves its
    /// file as its own Stop does. Returns the jobs it asked to stop, as they
    /// were; one already stopping is not among them. New jobs are taken.
    pub fn stop_all(&self) -> Vec<JobView> {
        let asked: Vec<JobView> = self
            .active()
            .into_iter()
            .filter(|j| j.state != JobState::Stopping)
            .filter(|j| self.stop(j.id).is_ok())
            .collect();
        if !asked.is_empty() {
            log::info!("every job asked to stop: jobs={}", asked.len());
        }
        asked
    }

    /// The app is closing: nothing new is taken (`JobError::Closed`), every
    /// job is stopped, and this waits until none is active or `timeout` has passed.
    pub async fn shutdown(&self, timeout: Duration) {
        let mut ended = self.inner.ended.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        self.inner.board.lock().closed = true;
        let active = self.active();
        if !active.is_empty() {
            log::info!(
                "jobs are stopped, the app is closing: active={}",
                active.len()
            );
        }
        for job in active {
            let _ = self.stop(job.id);
        }
        while !self.active().is_empty() {
            match tokio::time::timeout_at(deadline, ended.changed()).await {
                Ok(Ok(())) => {}
                _ => {
                    log::warn!(
                        "jobs did not end in time, the app closes anyway: active={}",
                        self.active().len()
                    );
                    break;
                }
            }
        }
    }
}

/// `first` and, when there is one, what was done before it.
fn join_text(first: &str, partial: &str) -> String {
    let partial = partial.trim();
    if partial.is_empty() {
        first.to_string()
    } else {
        format!("{first} {partial}")
    }
}

/// Runs `tool.run(args, approval, cancel)` as a queue job. `approval` is the
/// one the gate gave for this very call.
pub fn run_tool_in_background(
    board: &JobBoard,
    tool: Arc<dyn Tool>,
    args: Value,
    approval: Approval,
    title: String,
) -> Result<Submitted, JobError> {
    let kind = tool.spec().name;
    board.submit(
        JobSpec::queued(kind, title),
        Box::new(move |ctx: JobCtx| {
            Box::pin(async move { tool.run(args, approval, ctx.cancel).await })
        }),
    )
}

/// What the tool returns at once: the text for the model and the chat card.
pub fn started_output(s: &Submitted) -> ToolOutput {
    let ahead = if s.ahead > 0 {
        format!("Перед ним в очереди: {}. ", s.ahead)
    } else {
        String::new()
    };
    let text = format!(
        "{STARTED_PREFIX} задание №{} «{}». {ahead}Ход виден на карточке в чате, остановить — \
кнопкой «Стоп» на ней или bg_job_stop. Работа уже идёт сама: не вызывай этот инструмент снова \
и не проси подождать. Скажи пользователю одной фразой, что работа идёт в фоне и что ты сообщишь, \
когда она закончится.",
        s.job.id, s.job.title
    );
    ToolOutput::new(text).with_data(job_card(&s.job))
}

/// The chat card of a job: `{"kind":"bg_job","job":…}`.
pub fn job_card(job: &JobView) -> Value {
    json!({ "kind": "bg_job", "job": job })
}

/// Characters of an outcome shown in the list of jobs.
const LIST_TEXT: usize = 200;

fn shorten(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// One line of `bg_jobs` about a job.
fn job_line(job: &JobView) -> String {
    let note = if job.note.is_empty() {
        String::new()
    } else {
        format!(" ({})", job.note)
    };
    let said = job.text.trim();
    let outcome = if said.is_empty() || said == STOPPED_TEXT {
        String::new()
    } else {
        format!(" — {}", shorten(said, LIST_TEXT))
    };
    let (id, title) = (job.id, &job.title);
    match job.state {
        JobState::Queued => format!("№{id} в очереди: {title}"),
        JobState::Running if job.live => format!("№{id} идёт запись: {title}{note}"),
        JobState::Running => format!("№{id} идёт, {}%: {title}{note}", job.share),
        JobState::Stopping => format!("№{id} останавливается: {title}{note}"),
        JobState::Done => format!("№{id} готово: {title}{outcome}"),
        JobState::Failed => format!("№{id} не получилось: {title}{outcome}"),
        JobState::Stopped => format!("№{id} остановлено: {title}{outcome}"),
    }
}

/// Tool `bg_jobs`: what runs, what waits and how the last jobs ended.
pub struct BgJobsTool(pub JobBoard);

#[async_trait]
impl Tool for BgJobsTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "bg_jobs",
            "Фоновые задания с видео и звуком: что идёт, сколько сделано, что ждёт в очереди и чем кончились последние. Вызывай, когда спрашивают, как идёт сжатие, конвертация, расшифровка или запись.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть фоновые задания",
            "очередь заданий",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let jobs = self.0.list();
        if jobs.is_empty() {
            return Ok(ToolOutput::new("Фоновых заданий нет."));
        }
        let lines: Vec<String> = jobs.iter().map(job_line).collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

/// Longest wait of `bg_job_stop` for a queue job to end, and for a live one:
/// a recording is saved when stopped, which takes time.
const STOP_WAIT: Duration = Duration::from_secs(15);
const STOP_WAIT_LIVE: Duration = Duration::from_secs(185);

/// Tool `bg_job_stop`: stops one job.
pub struct BgJobStopTool(pub JobBoard);

impl BgJobStopTool {
    /// The job the call means: the one named by `id`, or the only active one.
    fn pick(&self, args: &Value) -> Result<JobView, ToolError> {
        let bad = |m: String| ToolError::BadArguments(m);
        let id = match args.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => {
                Some(n.as_u64().ok_or_else(|| bad("id — номер задания".into()))?)
            }
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(Value::String(s)) => Some(
                s.trim()
                    .trim_start_matches('№')
                    .trim()
                    .parse::<JobId>()
                    .map_err(|_| bad("id — номер задания".into()))?,
            ),
            Some(_) => return Err(bad("id — номер задания".into())),
        };
        let active = self.0.active();
        match id {
            Some(id) => active
                .into_iter()
                .find(|j| j.id == id)
                .ok_or_else(|| bad(format!("задания №{id} нет среди идущих"))),
            None => match active.len() {
                0 => Err(bad("фоновых заданий нет".into())),
                1 => Ok(active.into_iter().next().expect("one job")),
                _ => {
                    let names: Vec<String> = active
                        .iter()
                        .map(|j| format!("№{} {}", j.id, shorten(&j.title, 60)))
                        .collect();
                    Err(bad(format!(
                        "идёт несколько заданий, назови номер: {}",
                        names.join("; ")
                    )))
                }
            },
        }
    }
}

#[async_trait]
impl Tool for BgJobStopTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "bg_job_stop",
            "Остановить фоновое задание. Недоделанный файл удалится, готовые останутся; запись экрана при остановке сохраняется.",
            json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "Номер задания из bg_jobs; без него — единственное идущее или ждущее"
                    }
                }
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let job = self.pick(args)?;
        let (level, reason) = if job.live {
            (RiskLevel::Low, "запись остановится и сохранится")
        } else {
            (
                RiskLevel::Medium,
                "сделанная часть работы пропадёт, готовые файлы останутся",
            )
        };
        Ok(Assessment::new(
            level,
            format!("Остановить фоновое задание №{}", job.id),
            job.title,
            reason,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let job = self.pick(&args)?;
        let id = job.id;
        // This call tells how it ended: the app keeps quiet about it.
        self.0.set_silent(id, true);
        if let Err(e) = self.0.stop(id) {
            return Err(ToolError::Failed(e.to_string()));
        }
        let limit = if job.live { STOP_WAIT_LIVE } else { STOP_WAIT };
        let ended = tokio::select! {
            v = self.0.wait(id, limit) => v,
            _ = cancel.cancelled() => None,
        };
        let Some(ended) = ended else {
            self.0.set_silent(id, false);
            return Ok(ToolOutput::new(format!(
                "Останавливаю задание №{id}: скажу, когда закончу."
            )));
        };
        let said = ended.text.trim();
        let text = if said.is_empty() || said == STOPPED_TEXT {
            format!("Задание №{id} остановлено.")
        } else {
            format!("Задание №{id} остановлено. {said}")
        };
        let mut out = ToolOutput::new(text);
        out.data = ended.data;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::tools::progress_handle;

    const WAIT: Duration = Duration::from_secs(10);

    fn board() -> (JobBoard, tokio::sync::broadcast::Receiver<Event>) {
        let bus = EventBus::new(256);
        let rx = bus.subscribe();
        (JobBoard::new(bus), rx)
    }

    /// Work that ends with `result` once `gate` opens, or when it is asked to stop.
    fn gated(
        gate: Arc<tokio::sync::Notify>,
        result: Result<ToolOutput, ToolError>,
        log: Arc<Mutex<Vec<String>>>,
        name: &'static str,
    ) -> JobWork {
        Box::new(move |ctx: JobCtx| {
            Box::pin(async move {
                log.lock().push(format!("start {name}"));
                tokio::select! {
                    _ = gate.notified() => {}
                    _ = ctx.cancel.cancelled() => {
                        log.lock().push(format!("cancel {name}"));
                        return Err(ToolError::Cancelled);
                    }
                }
                log.lock().push(format!("end {name}"));
                result
            })
        })
    }

    fn quick(result: Result<ToolOutput, ToolError>) -> JobWork {
        Box::new(move |_ctx: JobCtx| Box::pin(async move { result }))
    }

    fn states(rx: &mut tokio::sync::broadcast::Receiver<Event>, id: JobId) -> Vec<JobState> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let Event::BgJobUpdated { job } = e {
                if job.id == id && out.last() != Some(&job.state) {
                    out.push(job.state);
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn queue_jobs_run_in_order_and_one_at_a_time() {
        let (b, mut rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let gates: Vec<_> = (0..3)
            .map(|_| Arc::new(tokio::sync::Notify::new()))
            .collect();
        let names = ["a", "b", "c"];
        let mut ids = Vec::new();
        for (gate, name) in gates.iter().zip(names) {
            let s = b
                .submit(
                    JobSpec::queued("video_compress", name),
                    gated(
                        gate.clone(),
                        Ok(ToolOutput::new(format!("готово {name}"))),
                        log.clone(),
                        name,
                    ),
                )
                .unwrap();
            ids.push((s.job.id, s.job.state, s.ahead));
        }
        assert_eq!(
            ids,
            vec![
                (1, JobState::Running, 0),
                (2, JobState::Queued, 1),
                (3, JobState::Queued, 2)
            ],
            "the first one is taken at once, the rest wait behind it"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*log.lock(), vec!["start a"], "only one works");
        let order: Vec<JobId> = b.list().iter().map(|j| j.id).collect();
        assert_eq!(order, vec![1, 2, 3]);

        // The gates open in the wrong order: the jobs still end in theirs.
        gates[2].notify_one();
        gates[1].notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*log.lock(), vec!["start a"]);
        gates[0].notify_one();
        for id in [1, 2, 3] {
            let v = b.wait(id, WAIT).await.unwrap();
            assert_eq!(v.state, JobState::Done);
            assert_eq!(v.share, 100, "a done job is at 100");
        }
        assert_eq!(
            *log.lock(),
            vec!["start a", "end a", "start b", "end b", "start c", "end c"]
        );
        assert_eq!(b.get(2).unwrap().text, "готово b");
        assert_eq!(
            states(&mut rx, 2),
            vec![JobState::Queued, JobState::Running, JobState::Done]
        );
        // Finished ones, newest first.
        let order: Vec<JobId> = b.list().iter().map(|j| j.id).collect();
        assert_eq!(order, vec![3, 2, 1]);
        assert!(b.active().is_empty());

        // The worker ended with the queue; a new job starts a new one.
        let s = b
            .submit(JobSpec::queued("t", "d"), quick(Ok(ToolOutput::new("d"))))
            .unwrap();
        assert_eq!((s.job.state, s.ahead), (JobState::Running, 0));
        assert_eq!(b.wait(s.job.id, WAIT).await.unwrap().state, JobState::Done);
    }

    #[tokio::test]
    async fn a_live_job_runs_beside_the_queue() {
        let (b, _rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (g1, g2) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let queued = b
            .submit(
                JobSpec::queued("av_convert", "q"),
                gated(g1.clone(), Ok(ToolOutput::new("q")), log.clone(), "q"),
            )
            .unwrap();
        let live = b
            .submit(
                JobSpec::live("screen_record", "rec", Duration::from_secs(1)),
                gated(g2.clone(), Ok(ToolOutput::new("rec")), log.clone(), "rec"),
            )
            .unwrap();
        assert!(live.job.live && live.job.state == JobState::Running);
        assert_eq!(live.ahead, 0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(*log.lock(), vec!["start q", "start rec"]);
        // A second queue job still waits for the first, not for the recording.
        let second = b
            .submit(JobSpec::queued("t", "w"), quick(Ok(ToolOutput::new("w"))))
            .unwrap();
        assert_eq!((second.job.state, second.ahead), (JobState::Queued, 1));
        g2.notify_one();
        assert_eq!(
            b.wait(live.job.id, WAIT).await.unwrap().state,
            JobState::Done
        );
        assert_eq!(b.get(queued.job.id).unwrap().state, JobState::Running);
        g1.notify_one();
        assert_eq!(
            b.wait(second.job.id, WAIT).await.unwrap().state,
            JobState::Done
        );
    }

    #[tokio::test]
    async fn a_stopped_queued_job_never_runs() {
        let (b, mut rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(tokio::sync::Notify::new());
        let first = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(gate.clone(), Ok(ToolOutput::new("a")), log.clone(), "a"),
            )
            .unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let second = b
            .submit(
                JobSpec::queued("t", "b"),
                Box::new(move |_ctx: JobCtx| {
                    flag.store(true, Ordering::SeqCst);
                    Box::pin(async { Ok(ToolOutput::new("b")) })
                }),
            )
            .unwrap();
        let stopped = b.stop(second.job.id).unwrap();
        assert_eq!(stopped.state, JobState::Stopped);
        assert_eq!(stopped.text, "Остановлено.");
        assert_eq!(
            b.wait(second.job.id, Duration::from_millis(10)).await,
            Some(stopped)
        );
        assert_eq!(
            b.stop(second.job.id),
            Err(JobError::Finished(second.job.id))
        );
        assert_eq!(b.stop(99), Err(JobError::NotFound(99)));
        gate.notify_one();
        b.wait(first.job.id, WAIT).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!ran.load(Ordering::SeqCst), "its work was dropped");
        assert_eq!(
            states(&mut rx, second.job.id),
            vec![JobState::Queued, JobState::Stopped]
        );
    }

    #[tokio::test]
    async fn stop_of_a_running_job_cancels_its_token() {
        let (b, mut rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(tokio::sync::Notify::new());
        let s = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(gate, Ok(ToolOutput::new("a")), log.clone(), "a"),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let v = b.stop(s.job.id).unwrap();
        assert_eq!(v.state, JobState::Stopping);
        assert_eq!(b.stop(s.job.id).unwrap().state, JobState::Stopping);
        let ended = b.wait(s.job.id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Stopped);
        assert_eq!(ended.text, "Остановлено.");
        assert_eq!(*log.lock(), vec!["start a", "cancel a"]);
        assert_eq!(
            states(&mut rx, s.job.id),
            vec![JobState::Running, JobState::Stopping, JobState::Stopped]
        );
    }

    /// Sets a flag when the work's future is dropped.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn work_that_ignores_the_stop_is_dropped_after_the_grace() {
        let (b, _rx) = board();
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = DropFlag(dropped.clone());
        let mut spec = JobSpec::queued("t", "deaf");
        spec.grace = Duration::from_millis(150);
        let s = b
            .submit(
                spec,
                Box::new(move |_ctx: JobCtx| {
                    Box::pin(async move {
                        let _guard = guard;
                        tokio::time::sleep(Duration::from_secs(600)).await;
                        Ok(ToolOutput::new("never"))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let asked = Instant::now();
        b.stop(s.job.id).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            b.get(s.job.id).unwrap().state,
            JobState::Stopping,
            "it has its grace"
        );
        assert!(!dropped.load(Ordering::SeqCst));
        let ended = b.wait(s.job.id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Stopped);
        assert!(asked.elapsed() >= Duration::from_millis(150));
        assert!(asked.elapsed() < Duration::from_secs(5));
        assert!(dropped.load(Ordering::SeqCst), "the future was dropped");
    }

    #[tokio::test]
    async fn work_that_ends_well_after_a_stop_is_done() {
        let (b, _rx) = board();
        // A recording: the stop makes it save the file and end well.
        let s = b
            .submit(
                JobSpec::live("screen_record", "rec", Duration::from_secs(5)),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.cancel.cancelled().await;
                        ctx.handle.set_note("сохраняю запись…");
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        Ok(ToolOutput::new("Запись сохранена.")
                            .with_data(json!({"kind": "av_files", "files": []})))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        b.stop(s.job.id).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mid = b.get(s.job.id).unwrap();
        assert_eq!(
            (mid.state, mid.note.as_str()),
            (JobState::Stopping, "сохраняю запись…"),
            "a stopping job still shows what it does"
        );
        let ended = b.wait(s.job.id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Done);
        assert_eq!(ended.text, "Запись сохранена.");
        assert_eq!(ended.data.unwrap()["kind"], "av_files");
    }

    #[tokio::test]
    async fn partial_results_survive_a_stop_and_a_failure() {
        let (b, _rx) = board();
        let card = json!({"kind": "av_files", "files": [{"path": "C:\\a.mp4", "bytes": 1}]});
        let kept = card.clone();
        let stopped = b
            .submit(
                JobSpec::queued("av_folder", "папка"),
                Box::new(move |ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.handle.set_partial("Готово 1 из 3.", Some(kept));
                        ctx.cancel.cancelled().await;
                        Err(ToolError::Cancelled)
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        b.stop(stopped.job.id).unwrap();
        let v = b.wait(stopped.job.id, WAIT).await.unwrap();
        assert_eq!(v.state, JobState::Stopped);
        assert_eq!(v.text, "Готово 1 из 3.");
        assert_eq!(v.data, Some(card.clone()));

        let kept = card.clone();
        let failed = b
            .submit(
                JobSpec::queued("av_folder", "папка"),
                Box::new(move |ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.handle.set_partial("Готово 1 из 3.", Some(kept));
                        Err(ToolError::Failed("диск полон".into()))
                    })
                }),
            )
            .unwrap();
        let v = b.wait(failed.job.id, WAIT).await.unwrap();
        assert_eq!(v.state, JobState::Failed);
        assert_eq!(v.text, "диск полон Готово 1 из 3.");
        assert_eq!(v.data, Some(card));

        let plain = b
            .submit(
                JobSpec::queued("t", "x"),
                quick(Err(ToolError::Failed(
                    "FFmpeg завершился с ошибкой: x".into(),
                ))),
            )
            .unwrap();
        let v = b.wait(plain.job.id, WAIT).await.unwrap();
        assert_eq!(
            (v.state, v.text.as_str(), v.data),
            (JobState::Failed, "FFmpeg завершился с ошибкой: x", None)
        );
    }

    #[tokio::test]
    async fn a_panic_in_the_work_is_a_failed_job_and_the_queue_goes_on() {
        async fn boom() -> Result<ToolOutput, ToolError> {
            panic!("boom")
        }
        let (b, _rx) = board();
        let boom = b
            .submit(
                JobSpec::queued("t", "boom"),
                Box::new(|_ctx: JobCtx| Box::pin(boom())),
            )
            .unwrap();
        let early = b
            .submit(
                JobSpec::queued("t", "early"),
                Box::new(|_ctx: JobCtx| -> JobFuture { panic!("before the future") }),
            )
            .unwrap();
        let next = b
            .submit(JobSpec::queued("t", "ok"), quick(Ok(ToolOutput::new("ok"))))
            .unwrap();
        for id in [boom.job.id, early.job.id] {
            let v = b.wait(id, WAIT).await.unwrap();
            assert_eq!(v.state, JobState::Failed);
            assert_eq!(v.text, "внутренняя ошибка задания");
        }
        assert_eq!(
            b.wait(next.job.id, WAIT).await.unwrap().state,
            JobState::Done
        );
    }

    #[tokio::test]
    async fn the_queue_is_full_at_ten_waiting_jobs() {
        let (b, _rx) = board();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let hold = b
            .submit(
                JobSpec::queued("t", "hold"),
                gated(gate.clone(), Ok(ToolOutput::new("")), log, "hold"),
            )
            .unwrap();
        for i in 0..MAX_QUEUED {
            let s = b
                .submit(
                    JobSpec::queued("t", format!("w{i}")),
                    quick(Ok(ToolOutput::new(""))),
                )
                .unwrap();
            assert_eq!(s.ahead, i + 1);
        }
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let over = b.submit(
            JobSpec::queued("t", "over"),
            Box::new(move |_ctx: JobCtx| {
                flag.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(ToolOutput::new("")) })
            }),
        );
        assert_eq!(over.err(), Some(JobError::QueueFull));
        assert_eq!(b.list().len(), MAX_QUEUED + 1, "nothing was added");
        // A recording is not a queue job: it still starts.
        let live = b
            .submit(
                JobSpec::live("screen_record", "rec", Duration::from_secs(1)),
                quick(Ok(ToolOutput::new("rec"))),
            )
            .unwrap();
        assert_eq!(live.job.state, JobState::Running);
        gate.notify_one();
        b.wait(hold.job.id, WAIT).await.unwrap();
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stop_all_stops_every_job_and_a_recording_saves_its_file() {
        let (b, _rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let never = Arc::new(tokio::sync::Notify::new());
        let running = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(never.clone(), Ok(ToolOutput::new("a")), log.clone(), "a"),
            )
            .unwrap();
        let waiting = b
            .submit(
                JobSpec::queued("t", "b"),
                gated(never.clone(), Ok(ToolOutput::new("b")), log.clone(), "b"),
            )
            .unwrap();
        let live = b
            .submit(
                JobSpec::live("screen_record", "rec", Duration::from_secs(5)),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.cancel.cancelled().await;
                        Ok(ToolOutput::new("Запись сохранена."))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let stopped = b.stop_all();
        let mut ids: Vec<JobId> = stopped.iter().map(|j| j.id).collect();
        ids.sort();
        assert_eq!(ids, vec![running.job.id, waiting.job.id, live.job.id]);
        assert_eq!(stopped.iter().filter(|j| j.live).count(), 1);
        let ended = b.wait(live.job.id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Done, "a recording is saved");
        b.wait(running.job.id, WAIT).await.unwrap();
        assert!(b.active().is_empty());
        assert_eq!(b.get(waiting.job.id).unwrap().state, JobState::Stopped);
        // The board stays open: a full stop is not the app closing.
        assert!(b
            .submit(JobSpec::queued("t", "c"), quick(Ok(ToolOutput::new(""))))
            .is_ok());
        // Nothing active: nothing to stop.
        let (empty, _rx) = board();
        assert!(empty.stop_all().is_empty());
    }

    #[tokio::test]
    async fn shutdown_closes_the_board_and_stops_every_job() {
        let (b, _rx) = board();
        let log = Arc::new(Mutex::new(Vec::new()));
        let never = Arc::new(tokio::sync::Notify::new());
        let running = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(never.clone(), Ok(ToolOutput::new("a")), log.clone(), "a"),
            )
            .unwrap();
        let waiting = b
            .submit(
                JobSpec::queued("t", "b"),
                gated(never.clone(), Ok(ToolOutput::new("b")), log.clone(), "b"),
            )
            .unwrap();
        let live = b
            .submit(
                JobSpec::live("screen_record", "rec", Duration::from_secs(5)),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.cancel.cancelled().await;
                        Ok(ToolOutput::new("Запись сохранена."))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        b.shutdown(WAIT).await;
        assert!(b.active().is_empty());
        assert_eq!(b.get(running.job.id).unwrap().state, JobState::Stopped);
        assert_eq!(b.get(waiting.job.id).unwrap().state, JobState::Stopped);
        assert_eq!(
            b.get(live.job.id).unwrap().state,
            JobState::Done,
            "a recording is saved, not thrown away"
        );
        assert_eq!(*log.lock(), vec!["start a", "cancel a"], "b never started");
        let late = b.submit(JobSpec::queued("t", "c"), quick(Ok(ToolOutput::new(""))));
        assert_eq!(late.err(), Some(JobError::Closed));
        // An empty board shuts down at once.
        let (empty, _rx) = board();
        let asked = Instant::now();
        empty.shutdown(WAIT).await;
        assert!(asked.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn shutdown_gives_up_at_its_timeout() {
        let (b, _rx) = board();
        let mut spec = JobSpec::queued("t", "deaf");
        spec.grace = Duration::from_secs(600);
        b.submit(
            spec,
            Box::new(|_ctx: JobCtx| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(600)).await;
                    Ok(ToolOutput::new(""))
                })
            }),
        )
        .unwrap();
        let asked = Instant::now();
        b.shutdown(Duration::from_millis(100)).await;
        assert!(asked.elapsed() < Duration::from_secs(5));
        assert_eq!(b.active().len(), 1);
    }

    #[tokio::test]
    async fn wait_gives_the_ended_job_or_nothing() {
        let (b, _rx) = board();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let s = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(gate.clone(), Ok(ToolOutput::new("итог")), log, "a"),
            )
            .unwrap();
        assert_eq!(b.wait(s.job.id, Duration::from_millis(40)).await, None);
        assert_eq!(b.wait(77, WAIT).await, None, "an unknown job");
        let waiter = {
            let b = b.clone();
            tokio::spawn(async move { b.wait(s.job.id, WAIT).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_one();
        let v = waiter.await.unwrap().unwrap();
        assert_eq!((v.state, v.text.as_str()), (JobState::Done, "итог"));
        assert_eq!(b.wait(s.job.id, WAIT).await, Some(v), "already ended");
    }

    #[tokio::test]
    async fn silence_shows_in_the_final_event() {
        let (b, mut rx) = board();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let s = b
            .submit(
                JobSpec::queued("t", "a"),
                gated(gate, Ok(ToolOutput::new("a")), log, "a"),
            )
            .unwrap();
        assert!(!s.job.silent);
        b.set_silent(s.job.id, true);
        b.stop(s.job.id).unwrap();
        b.wait(s.job.id, WAIT).await.unwrap();
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            if let Event::BgJobUpdated { job } = e {
                last = Some(job);
            }
        }
        let last = last.unwrap();
        assert_eq!(last.state, JobState::Stopped);
        assert!(last.silent);
    }

    #[tokio::test]
    async fn share_events_are_throttled_and_end_with_100() {
        let (b, mut rx) = board();
        let s = b
            .submit(
                JobSpec::queued("video_compress", "сжать"),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        // What a tool does: the progress of its call.
                        let p = progress_handle();
                        assert_eq!(job_handle().map(|h| h.id()), Some(ctx.handle.id()));
                        for share in 1..=40u8 {
                            p.set(share);
                        }
                        ctx.handle.set_note("1 из 2: а.mp4");
                        ctx.handle.set_note("1 из 2: а.mp4");
                        Ok(ToolOutput::new("готово"))
                    })
                }),
            )
            .unwrap();
        b.wait(s.job.id, WAIT).await.unwrap();
        assert!(job_handle().is_none(), "no job outside its work");
        let mut shares = Vec::new();
        let mut notes = 0;
        while let Ok(e) = rx.try_recv() {
            if let Event::BgJobUpdated { job } = e {
                if shares.last() != Some(&job.share) {
                    shares.push(job.share);
                }
                if job.state == JobState::Running && job.note == "1 из 2: а.mp4" {
                    notes += 1;
                }
            }
        }
        assert_eq!(shares, vec![0, 1, 100], "forty calls at once are one event");
        assert_eq!(notes, 1, "the same note is published once");
    }

    #[tokio::test]
    async fn changes_after_the_end_are_ignored() {
        let (b, mut rx) = board();
        let kept: Arc<Mutex<Option<JobHandle>>> = Arc::new(Mutex::new(None));
        let slot = kept.clone();
        let s = b
            .submit(
                JobSpec::queued("t", "a"),
                Box::new(move |ctx: JobCtx| {
                    *slot.lock() = Some(ctx.handle.clone());
                    Box::pin(async { Ok(ToolOutput::new("a")) })
                }),
            )
            .unwrap();
        b.wait(s.job.id, WAIT).await.unwrap();
        while rx.try_recv().is_ok() {}
        let handle = kept.lock().clone().unwrap();
        handle.set_share(10);
        handle.set_note("поздно");
        handle.set_partial("поздно", None);
        assert!(rx.try_recv().is_err());
        let v = b.get(s.job.id).unwrap();
        assert_eq!((v.share, v.note.as_str()), (100, ""));
    }

    #[tokio::test]
    async fn only_the_last_finished_jobs_are_kept() {
        let (b, _rx) = board();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        b.submit(
            JobSpec::queued("t", "hold"),
            gated(gate, Ok(ToolOutput::new("")), log, "hold"),
        )
        .unwrap();
        let mut last = 0;
        for i in 0..KEPT_FINISHED + 2 {
            // Stopped while it waits: it ends at once.
            let s = b
                .submit(
                    JobSpec::queued("t", format!("j{i}")),
                    quick(Ok(ToolOutput::new(""))),
                )
                .unwrap();
            b.stop(s.job.id).unwrap();
            last = s.job.id;
        }
        let list = b.list();
        assert_eq!(list.len(), 1 + KEPT_FINISHED);
        assert_eq!(list[1].id, last, "the newest finished one comes first");
        assert!(b.get(2).is_none(), "the oldest finished ones are gone");
    }

    #[tokio::test]
    async fn a_job_remembers_whether_its_turn_came_from_telegram() {
        let (b, mut rx) = board();
        // Nobody says: a job of this computer.
        let local = b
            .submit(JobSpec::queued("t", "a"), quick(Ok(ToolOutput::new("a"))))
            .unwrap()
            .job;
        assert!(!local.remote);

        let from_telegram = Arc::new(AtomicBool::new(true));
        let asks = from_telegram.clone();
        b.follow_origin(move || asks.load(Ordering::SeqCst));
        let remote = b
            .submit(JobSpec::queued("t", "b"), quick(Ok(ToolOutput::new("b"))))
            .unwrap()
            .job;
        assert!(remote.remote);
        // The turn that runs when the job ends is another one: the job keeps
        // what was true when it was made.
        from_telegram.store(false, Ordering::SeqCst);
        let ended = b.wait(remote.id, WAIT).await.expect("the job ends");
        assert_eq!(ended.state, JobState::Done);
        assert!(ended.remote);
        assert!(!b.wait(local.id, WAIT).await.unwrap().remote);
        // Every event of the job carries it, the last one too.
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            if let Event::BgJobUpdated { job } = e {
                if job.id == remote.id {
                    assert!(job.remote);
                    last = Some(job.state);
                }
            }
        }
        assert_eq!(last, Some(JobState::Done));
        // A job made now is of this computer again.
        let later = b
            .submit(JobSpec::queued("t", "c"), quick(Ok(ToolOutput::new("c"))))
            .unwrap()
            .job;
        assert!(!later.remote);
        // It is not a part of the JSON the interface gets.
        assert!(serde_json::to_value(&ended)
            .unwrap()
            .get("remote")
            .is_none());
        b.shutdown(WAIT).await;
    }

    #[test]
    fn job_view_json_skips_empty_fields() {
        let mut v = JobView {
            id: 3,
            kind: "video_compress".into(),
            title: "Сжать «отпуск.mp4»".into(),
            state: JobState::Running,
            live: false,
            share: 42,
            note: String::new(),
            text: String::new(),
            data: None,
            silent: false,
            remote: false,
        };
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({
                "id": 3, "kind": "video_compress", "title": "Сжать «отпуск.mp4»",
                "state": "running", "live": false, "share": 42
            })
        );
        assert_eq!(
            serde_json::from_value::<JobView>(serde_json::to_value(&v).unwrap()).unwrap(),
            v
        );
        v.state = JobState::Done;
        v.note = "3 из 12: клип.mp4".into();
        v.text = "Готово.".into();
        v.data = Some(json!({"kind": "av_files", "files": []}));
        v.silent = true;
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["state"], "done");
        assert_eq!(j["note"], "3 из 12: клип.mp4");
        assert_eq!(j["text"], "Готово.");
        assert_eq!(j["data"]["kind"], "av_files");
        assert_eq!(j["silent"], true);
        for s in [
            JobState::Queued,
            JobState::Running,
            JobState::Stopping,
            JobState::Done,
            JobState::Failed,
            JobState::Stopped,
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
            assert_eq!(
                s.active(),
                matches!(s.as_str(), "queued" | "running" | "stopping")
            );
        }
    }

    #[tokio::test]
    async fn started_output_names_the_job_and_carries_its_card() {
        let (b, _rx) = board();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let first = b
            .submit(
                JobSpec::queued("video_compress", "Сжать «а.mp4»"),
                gated(gate.clone(), Ok(ToolOutput::new("")), log.clone(), "a"),
            )
            .unwrap();
        let out = started_output(&first);
        assert!(
            out.text
                .starts_with("Запущено в фоне: задание №1 «Сжать «а.mp4»». Ход виден"),
            "{}",
            out.text
        );
        assert!(out.text.starts_with(STARTED_PREFIX));
        assert!(out.text.contains("не вызывай этот инструмент снова"));
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "bg_job");
        assert_eq!(card["job"]["id"], 1);
        assert_eq!(card["job"]["state"], "running");
        let second = b
            .submit(
                JobSpec::queued("av_convert", "Перевести «б.mkv»"),
                gated(gate.clone(), Ok(ToolOutput::new("")), log, "b"),
            )
            .unwrap();
        let out = started_output(&second);
        assert!(
            out.text
                .contains("«Перевести «б.mkv»». Перед ним в очереди: 1. Ход виден"),
            "{}",
            out.text
        );
        assert_eq!(out.data.unwrap()["job"]["state"], "queued");
        b.shutdown(WAIT).await;
    }

    /// A tool that counts its runs and gives back what it got.
    struct Probe(Arc<AtomicUsize>);

    #[async_trait]
    impl Tool for Probe {
        fn spec(&self) -> ToolSpec {
            spec(
                "probe_tool",
                "x",
                json!({ "type": "object", "properties": {} }),
            )
        }
        fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
            Ok(Assessment::new(RiskLevel::Medium, "Проба", "x", "x"))
        }
        async fn run(
            &self,
            args: Value,
            _approval: Approval,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, ToolError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            progress_handle().set(50);
            Ok(ToolOutput::new(format!("сделано {}", args["n"])))
        }
    }

    #[tokio::test]
    async fn a_tool_runs_in_the_background_with_its_approval() {
        let (b, _rx) = board();
        let runs = Arc::new(AtomicUsize::new(0));
        let s = run_tool_in_background(
            &b,
            Arc::new(Probe(runs.clone())),
            json!({ "n": 7 }),
            Approval::for_tests(),
            "Проба".into(),
        )
        .unwrap();
        assert_eq!(s.job.kind, "probe_tool");
        assert_eq!(s.job.title, "Проба");
        let v = b.wait(s.job.id, WAIT).await.unwrap();
        assert_eq!((v.state, v.text.as_str()), (JobState::Done, "сделано 7"));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bg_jobs_lists_every_job_in_words() {
        async fn run(tool: &BgJobsTool) -> ToolOutput {
            tool.run(json!({}), Approval::for_tests(), CancellationToken::new())
                .await
                .unwrap()
        }
        let (b, _rx) = board();
        let tool = BgJobsTool(b.clone());
        assert_eq!(tool.assess(&json!({})).unwrap().level, RiskLevel::Low);
        let out = run(&tool).await;
        assert_eq!(out.text, "Фоновых заданий нет.");
        assert!(out.data.is_none());

        let done = b
            .submit(
                JobSpec::queued("av_convert", "Перевести «а.mkv»"),
                quick(Ok(ToolOutput::new("Готово: C:\\v\\а.mp4, 1,2 МБ."))),
            )
            .unwrap();
        b.wait(done.job.id, WAIT).await.unwrap();
        let failed = b
            .submit(
                JobSpec::queued("av_convert", "Перевести «б.mkv»"),
                quick(Err(ToolError::Failed("нет кодировщика".into()))),
            )
            .unwrap();
        b.wait(failed.job.id, WAIT).await.unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let running = b
            .submit(
                JobSpec::queued("video_compress", "Сжать «в.mp4»"),
                Box::new({
                    let gate = gate.clone();
                    move |ctx: JobCtx| {
                        Box::pin(async move {
                            ctx.handle.set_share(42);
                            ctx.handle.set_note("проход 1");
                            tokio::select! {
                                _ = gate.notified() => Ok(ToolOutput::new("")),
                                _ = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
                            }
                        })
                    }
                }),
            )
            .unwrap();
        let queued = b
            .submit(
                JobSpec::queued("av_folder", "Сжать 12 видео"),
                gated(gate.clone(), Ok(ToolOutput::new("")), log.clone(), "q"),
            )
            .unwrap();
        let stopped = b
            .submit(
                JobSpec::queued("av_convert", "Перевести «г.mkv»"),
                quick(Ok(ToolOutput::new(""))),
            )
            .unwrap();
        b.stop(stopped.job.id).unwrap();
        let live = b
            .submit(
                JobSpec::live("screen_record", "Записать экран", Duration::from_secs(1)),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.handle.set_note("2:15 из 30:00");
                        ctx.cancel.cancelled().await;
                        Ok(ToolOutput::new("Запись сохранена."))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let out = run(&tool).await;
        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(
            lines,
            vec![
                format!("№{} идёт, 42%: Сжать «в.mp4» (проход 1)", running.job.id).as_str(),
                format!(
                    "№{} идёт запись: Записать экран (2:15 из 30:00)",
                    live.job.id
                )
                .as_str(),
                format!("№{} в очереди: Сжать 12 видео", queued.job.id).as_str(),
                format!("№{} остановлено: Перевести «г.mkv»", stopped.job.id).as_str(),
                format!(
                    "№{} не получилось: Перевести «б.mkv» — нет кодировщика",
                    failed.job.id
                )
                .as_str(),
                format!(
                    "№{} готово: Перевести «а.mkv» — Готово: C:\\v\\а.mp4, 1,2 МБ.",
                    done.job.id
                )
                .as_str(),
            ]
        );
        b.shutdown(WAIT).await;
    }

    #[test]
    fn a_long_outcome_is_cut_in_the_list() {
        let job = JobView {
            id: 1,
            kind: "av_transcribe".into(),
            title: "Расшифровать".into(),
            state: JobState::Done,
            live: false,
            share: 100,
            note: String::new(),
            text: "я".repeat(500),
            data: None,
            silent: false,
            remote: false,
        };
        let line = job_line(&job);
        assert_eq!(
            line.chars().count(),
            "№1 готово: Расшифровать — ".chars().count() + 201
        );
        assert!(line.ends_with('…'));
    }

    #[tokio::test]
    async fn bg_job_stop_picks_the_job_and_tells_its_risk() {
        let (b, _rx) = board();
        let tool = BgJobStopTool(b.clone());
        assert_eq!(
            tool.assess(&json!({})),
            Err(ToolError::BadArguments("фоновых заданий нет".into()))
        );
        assert_eq!(
            tool.assess(&json!({ "id": 5 })),
            Err(ToolError::BadArguments(
                "задания №5 нет среди идущих".into()
            ))
        );
        let gate = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let queue = b
            .submit(
                JobSpec::queued("video_compress", "Сжать «а.mp4»"),
                gated(gate.clone(), Ok(ToolOutput::new("")), log.clone(), "a"),
            )
            .unwrap();
        let a = tool.assess(&json!({})).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(a.summary, "Остановить фоновое задание №1");
        assert_eq!(a.target, "Сжать «а.mp4»");
        assert_eq!(
            a.reason,
            "сделанная часть работы пропадёт, готовые файлы останутся"
        );
        assert_eq!(a.scope, "*");

        let live = b
            .submit(
                JobSpec::live("screen_record", "Записать экран", Duration::from_secs(5)),
                Box::new(|ctx: JobCtx| {
                    Box::pin(async move {
                        ctx.cancel.cancelled().await;
                        Ok(ToolOutput::new("Запись сохранена: C:\\v\\з.mp4.")
                            .with_data(json!({"kind": "av_files", "files": []})))
                    })
                }),
            )
            .unwrap();
        match tool.assess(&json!({})) {
            Err(ToolError::BadArguments(m)) => assert_eq!(
                m,
                "идёт несколько заданий, назови номер: №1 Сжать «а.mp4»; №2 Записать экран"
            ),
            other => panic!("{other:?}"),
        }
        let a = tool.assess(&json!({ "id": live.job.id })).unwrap();
        assert_eq!(a.level, RiskLevel::Low, "a recording is saved when stopped");
        assert_eq!(a.reason, "запись остановится и сохранится");
        assert_eq!(
            tool.assess(&json!({ "id": "№1" })).unwrap().level,
            RiskLevel::Medium
        );
        assert!(tool.assess(&json!({ "id": "первое" })).is_err());

        // The recording ends well and gives its card to the call.
        let out = tool
            .run(
                json!({ "id": live.job.id }),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            out.text,
            "Задание №2 остановлено. Запись сохранена: C:\\v\\з.mp4."
        );
        assert_eq!(out.data.unwrap()["kind"], "av_files");
        assert!(
            b.get(live.job.id).unwrap().silent,
            "the call told it itself"
        );

        // The queue job is the only one left: no id is needed.
        let out = tool
            .run(json!({}), Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.text, "Задание №1 остановлено.");
        assert!(out.data.is_none());
        assert_eq!(b.get(queue.job.id).unwrap().state, JobState::Stopped);
        assert_eq!(
            tool.assess(&json!({ "id": 1 })),
            Err(ToolError::BadArguments(
                "задания №1 нет среди идущих".into()
            ))
        );
    }

    #[tokio::test]
    async fn bg_job_stop_does_not_wait_for_ever() {
        let (b, _rx) = board();
        let tool = BgJobStopTool(b.clone());
        let mut spec = JobSpec::queued("t", "deaf");
        spec.grace = Duration::from_secs(600);
        let s = b
            .submit(
                spec,
                Box::new(|_ctx: JobCtx| {
                    Box::pin(async {
                        tokio::time::sleep(Duration::from_secs(600)).await;
                        Ok(ToolOutput::new(""))
                    })
                }),
            )
            .unwrap();
        // «Стоп» of the chat ends the wait of the call, not the job's own stop.
        let cancel = CancellationToken::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            stopper.cancel();
        });
        let out = tool
            .run(json!({}), Approval::for_tests(), cancel)
            .await
            .unwrap();
        assert_eq!(out.text, "Останавливаю задание №1: скажу, когда закончу.");
        let v = b.get(s.job.id).unwrap();
        assert_eq!(v.state, JobState::Stopping);
        assert!(!v.silent, "the app will tell about the end");
    }
}
