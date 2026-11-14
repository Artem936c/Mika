// Background jobs: long work with video and sound that outlives a reply, and screen recording.
// Pure functions over what Rust sends. Nothing here counts time or progress.

import type { MessageKey } from './i18n';

export type JobState = 'queued' | 'running' | 'stopping' | 'done' | 'failed' | 'stopped';

export interface JobView {
  id: number;
  /** Name of the tool that started it. */
  kind: string;
  /** What it does, in the words of the confirm card. */
  title: string;
  state: JobState;
  /** A recording: it runs beside the queue and has no known amount of work. */
  live: boolean;
  /** 0..100 */
  share: number;
  note?: string;
  text?: string;
  /** Raw result card; the chat's `cardData` reads it. */
  data?: unknown;
}

/** How far along a state is: a job only moves forward through them. */
const RANK: Record<JobState, number> = { queued: 0, running: 1, stopping: 2, done: 3, failed: 3, stopped: 3 };

const isState = (v: unknown): v is JobState => typeof v === 'string' && Object.prototype.hasOwnProperty.call(RANK, v);

/** A job from an event or a card, or null when the shape is wrong. */
export function parseJob(v: unknown): JobView | null {
  if (typeof v !== 'object' || v === null) return null;
  const j = v as Record<string, unknown>;
  if (typeof j.id !== 'number' || !Number.isSafeInteger(j.id) || j.id < 0) return null;
  if (typeof j.kind !== 'string' || !j.kind || typeof j.title !== 'string') return null;
  if (!isState(j.state) || typeof j.live !== 'boolean') return null;
  if (typeof j.share !== 'number' || !Number.isFinite(j.share)) return null;
  const job: JobView = {
    id: j.id,
    kind: j.kind,
    title: j.title,
    state: j.state,
    live: j.live,
    share: Math.max(0, Math.min(100, Math.round(j.share))),
  };
  if (typeof j.note === 'string' && j.note) job.note = j.note;
  if (typeof j.text === 'string' && j.text) job.text = j.text;
  if (j.data !== undefined && j.data !== null) job.data = j.data;
  return job;
}

/** Waiting, working or being stopped: the job can still be stopped by the user. */
export function jobActive(job: JobView): boolean {
  return RANK[job.state] < RANK.done;
}

/** Label of the state: `bgjob.state_*`; a live job gives `rec.live` while running and `rec.saving` while stopping. */
export function jobStateKey(job: JobView): MessageKey {
  if (job.live && job.state === 'running') return 'rec.live';
  if (job.live && job.state === 'stopping') return 'rec.saving';
  return `bgjob.state_${job.state}`;
}

/**
 * Whether the outcome of a job says nothing beyond the label of its state: a job
 * stopped before it made anything ends with the very word that the label shows.
 */
export function repeatsLabel(text: string, label: string): boolean {
  const bare = (s: string) => s.trim().replace(/[.!…]+$/, '').toLowerCase();
  return bare(text) === bare(label);
}

function updatedJob(event: unknown): JobView | null {
  if (typeof event !== 'object' || event === null) return null;
  const e = event as { type?: unknown; job?: unknown };
  return e.type === 'bg_job_updated' ? parseJob(e.job) : null;
}

/** Id of the recording that runs now, or null: follows `bg_job_updated` of live jobs. */
export function reduceRecording(current: number | null, event: unknown): number | null {
  const job = updatedJob(event);
  if (!job || !job.live) return current;
  if (jobActive(job)) return job.id;
  return current === job.id ? null : current;
}

/** Newest state of each job the events told about, by id. */
export type JobsSeen = Readonly<Record<number, JobView>>;

/** More jobs than the board keeps (ten waiting, twenty finished, the running ones). */
const SEEN_KEPT = 64;

/**
 * Remembers the last `bg_job_updated` of every job. A chat card is made from the
 * answer of a tool or from a list, and either can be older than an event that
 * came before it: without this memory such a card would show a state that never changes.
 */
export function reduceJobsSeen(seen: JobsSeen, event: unknown): JobsSeen {
  const job = updatedJob(event);
  if (!job) return seen;
  const next: Record<number, JobView> = { ...seen, [job.id]: job };
  const ids = Object.keys(next).map(Number);
  if (ids.length > SEEN_KEPT) {
    ids.sort((a, b) => a - b);
    for (const id of ids.slice(0, ids.length - SEEN_KEPT)) delete next[id];
  }
  return next;
}

/**
 * `job`, or the state an event told about when that one is not behind it: the
 * states only move forward and the share only grows. A tie goes to the event.
 * The answer of a tool holds the job as it was submitted, and a note set right
 * after that (a recording that waits because its window is not in front) comes
 * in an event with the same state and share; the job then tells nothing more
 * until something changes, so the card would never show the note.
 */
export function newestJob(job: JobView, seen: JobsSeen): JobView {
  const known = seen[job.id];
  if (!known) return job;
  if (RANK[known.state] !== RANK[job.state]) return RANK[known.state] > RANK[job.state] ? known : job;
  return job.share > known.share ? job : known;
}

/** Jobs of a list (`bg_jobs_list`), each in its newest known state; entries of a wrong shape are dropped. */
export function listJobs(jobs: unknown, seen: JobsSeen = {}): JobView[] {
  if (!Array.isArray(jobs)) return [];
  return jobs
    .map(parseJob)
    .filter((j): j is JobView => j !== null)
    .map((j) => newestJob(j, seen));
}

/** Id of the recording that runs now, from the list `bg_jobs_list` returns. */
export function recordingOf(jobs: unknown): number | null {
  return listJobs(jobs).find((j) => j.live && jobActive(j))?.id ?? null;
}
