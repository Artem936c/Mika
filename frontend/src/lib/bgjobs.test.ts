import { describe, expect, it } from 'vitest';
import {
  jobActive,
  jobStateKey,
  listJobs,
  newestJob,
  parseJob,
  recordingOf,
  repeatsLabel,
  reduceJobsSeen,
  reduceRecording,
  type JobState,
  type JobView,
  type JobsSeen,
} from './bgjobs';
import { isMessageKey } from './i18n';

const job = (over: Partial<JobView> = {}): JobView => ({
  id: 3,
  kind: 'video_compress',
  title: 'Сжать «отпуск.mp4»',
  state: 'running',
  live: false,
  share: 42,
  ...over,
});

const rec = (over: Partial<JobView> = {}): JobView => job({ id: 5, kind: 'screen_record', title: 'Записать экран', live: true, share: 7, ...over });

const updated = (j: unknown) => ({ type: 'bg_job_updated', job: j });

describe('parseJob', () => {
  it('reads a job as Rust sends it', () => {
    expect(parseJob({ id: 3, kind: 'video_compress', title: 'Сжать', state: 'running', live: false, share: 42 })).toEqual({
      id: 3,
      kind: 'video_compress',
      title: 'Сжать',
      state: 'running',
      live: false,
      share: 42,
    });
  });

  it('keeps the note, the text and the raw card, and drops empty ones', () => {
    const data = { kind: 'av_files', files: [{ path: 'C:\\v\\a.mp4', bytes: 10 }] };
    expect(parseJob({ ...job({ state: 'done', share: 100 }), note: '3 из 12: день3.mp4', text: 'Готово', data })).toEqual({
      ...job({ state: 'done', share: 100 }),
      note: '3 из 12: день3.mp4',
      text: 'Готово',
      data,
    });
    const bare = parseJob({ ...job(), note: '', text: '', data: null, silent: true });
    expect(bare).toEqual(job());
    expect(bare).not.toHaveProperty('note');
    expect(bare).not.toHaveProperty('data');
  });

  it('clamps and rounds the share', () => {
    expect(parseJob(job({ share: 140 }))?.share).toBe(100);
    expect(parseJob(job({ share: -3 }))?.share).toBe(0);
    expect(parseJob(job({ share: 33.6 }))?.share).toBe(34);
  });

  it('refuses wrong shapes', () => {
    expect(parseJob(null)).toBeNull();
    expect(parseJob('job')).toBeNull();
    expect(parseJob([])).toBeNull();
    expect(parseJob({})).toBeNull();
    expect(parseJob({ ...job(), id: '3' })).toBeNull();
    expect(parseJob({ ...job(), id: -1 })).toBeNull();
    expect(parseJob({ ...job(), id: 1.5 })).toBeNull();
    expect(parseJob({ ...job(), kind: '' })).toBeNull();
    expect(parseJob({ ...job(), title: 7 })).toBeNull();
    expect(parseJob({ ...job(), state: 'paused' })).toBeNull();
    expect(parseJob({ ...job(), live: 'yes' })).toBeNull();
    expect(parseJob({ ...job(), share: '42' })).toBeNull();
    expect(parseJob({ ...job(), share: Number.NaN })).toBeNull();
  });
});

describe('job states', () => {
  it('queued, running and stopping are active', () => {
    const active = (state: JobState) => jobActive(job({ state }));
    expect(['queued', 'running', 'stopping', 'done', 'failed', 'stopped'].map((s) => active(s as JobState))).toEqual([
      true,
      true,
      true,
      false,
      false,
      false,
    ]);
  });

  it('every state has a label in the locale file', () => {
    for (const state of ['queued', 'running', 'stopping', 'done', 'failed', 'stopped'] as const) {
      expect(jobStateKey(job({ state }))).toBe(`bgjob.state_${state}`);
      expect(isMessageKey(jobStateKey(job({ state }))), state).toBe(true);
    }
  });

  it('a recording has its own words while it runs and while it is saved', () => {
    expect(jobStateKey(rec())).toBe('rec.live');
    expect(jobStateKey(rec({ state: 'stopping' }))).toBe('rec.saving');
    expect(jobStateKey(rec({ state: 'done' }))).toBe('bgjob.state_done');
    expect(jobStateKey(rec({ state: 'failed' }))).toBe('bgjob.state_failed');
    expect(isMessageKey('rec.live') && isMessageKey('rec.saving')).toBe(true);
  });
});

describe('repeatsLabel', () => {
  it('finds an outcome that only repeats the label of the state', () => {
    expect(repeatsLabel('Остановлено.', 'остановлено')).toBe(true);
    expect(repeatsLabel(' остановлено ', 'остановлено')).toBe(true);
    expect(repeatsLabel('Готово!', 'готово')).toBe(true);
  });

  it('keeps an outcome that says more', () => {
    expect(repeatsLabel('Готово 3 из 12: результаты в C:\\v', 'остановлено')).toBe(false);
    expect(repeatsLabel('Остановлено. Готово 3 из 12', 'остановлено')).toBe(false);
    expect(repeatsLabel('', 'остановлено')).toBe(false);
  });
});

describe('reduceRecording', () => {
  it('follows a recording from its start through the stop to the saved file', () => {
    let now: number | null = null;
    now = reduceRecording(now, updated(rec()));
    expect(now).toBe(5);
    now = reduceRecording(now, updated(rec({ note: '0:05 из 30:00' })));
    expect(now).toBe(5);
    now = reduceRecording(now, updated(rec({ state: 'stopping', note: 'сохраняю запись…' })));
    expect(now).toBe(5);
    now = reduceRecording(now, updated(rec({ state: 'done', share: 100, text: 'Запись сохранена' })));
    expect(now).toBeNull();
  });

  it('a failed or stopped recording clears it too', () => {
    expect(reduceRecording(5, updated(rec({ state: 'failed', text: 'ничего не записалось' })))).toBeNull();
    expect(reduceRecording(5, updated(rec({ state: 'stopped' })))).toBeNull();
  });

  it('jobs of the queue never touch it', () => {
    expect(reduceRecording(null, updated(job()))).toBeNull();
    expect(reduceRecording(5, updated(job({ id: 5, state: 'done' })))).toBe(5);
  });

  it('the end of another recording leaves the running one', () => {
    expect(reduceRecording(7, updated(rec({ id: 5, state: 'done' })))).toBe(7);
  });

  it('ignores other events and wrong shapes', () => {
    expect(reduceRecording(5, null)).toBe(5);
    expect(reduceRecording(5, { type: 'state_changed', to: 'idle' })).toBe(5);
    expect(reduceRecording(5, updated({ id: 5, live: true }))).toBe(5);
    expect(reduceRecording(null, { type: 'tool_finished', job: rec() })).toBeNull();
  });
});

describe('lists of jobs', () => {
  it('recordingOf finds the recording that runs', () => {
    expect(recordingOf([job(), rec()])).toBe(5);
    expect(recordingOf([job(), rec({ state: 'stopping' })])).toBe(5);
    expect(recordingOf([job(), rec({ state: 'done' })])).toBeNull();
    expect(recordingOf([])).toBeNull();
    expect(recordingOf(null)).toBeNull();
    expect(recordingOf('jobs')).toBeNull();
  });

  it('listJobs drops entries of a wrong shape', () => {
    expect(listJobs([job(), null, { id: 9 }, rec()])).toEqual([job(), rec()]);
    expect(listJobs({ jobs: [] })).toEqual([]);
  });

  it('listJobs takes the newer state an event told about', () => {
    const seen: JobsSeen = { 3: job({ state: 'done', share: 100, text: 'Готово' }) };
    expect(listJobs([job(), rec()], seen)).toEqual([job({ state: 'done', share: 100, text: 'Готово' }), rec()]);
    expect(recordingOf(listJobs([rec()], { 5: rec({ state: 'done' }) }))).toBeNull();
  });
});

describe('newest state of a job', () => {
  it('remembers the last event of every job', () => {
    let seen: JobsSeen = {};
    seen = reduceJobsSeen(seen, updated(job({ share: 10 })));
    seen = reduceJobsSeen(seen, updated(rec()));
    seen = reduceJobsSeen(seen, updated(job({ share: 20 })));
    expect(seen).toEqual({ 3: job({ share: 20 }), 5: rec() });
  });

  it('ignores other events and wrong shapes', () => {
    const seen: JobsSeen = { 3: job() };
    expect(reduceJobsSeen(seen, { type: 'tool_finished', job: job({ id: 4 }) })).toBe(seen);
    expect(reduceJobsSeen(seen, updated({ id: 4 }))).toBe(seen);
    expect(reduceJobsSeen(seen, 'x')).toBe(seen);
  });

  it('forgets the oldest jobs so the memory stays small', () => {
    let seen: JobsSeen = {};
    for (let id = 1; id <= 70; id += 1) seen = reduceJobsSeen(seen, updated(job({ id })));
    const ids = Object.keys(seen).map(Number);
    expect(ids).toHaveLength(64);
    expect(Math.min(...ids)).toBe(7);
    expect(Math.max(...ids)).toBe(70);
  });

  it('a state further along wins, then the larger share; a tie keeps the card', () => {
    const queued = job({ state: 'queued', share: 0 });
    const running = job({ share: 0 });
    expect(newestJob(queued, { 3: running })).toBe(running);
    expect(newestJob(running, { 3: queued })).toBe(running);
    const done = job({ state: 'done', share: 100 });
    expect(newestJob(job({ share: 90 }), { 3: done })).toBe(done);
    expect(newestJob(done, { 3: job({ share: 90 }) })).toBe(done);
    const further = job({ share: 60 });
    expect(newestJob(job({ share: 40 }), { 3: further })).toBe(further);
    const mine = job({ share: 60 });
    expect(newestJob(mine, { 3: job({ share: 40 }) })).toBe(mine);
    // A tie goes to the event: it may carry a note the answer of the tool lacks.
    const noted = job({ share: 60, note: 'x' });
    expect(newestJob(mine, { 3: noted })).toBe(noted);
    const paused = rec({ share: 0, note: 'пауза: окно не на переднем плане' });
    expect(newestJob(rec({ share: 0 }), { 5: paused })).toBe(paused);
    expect(newestJob(mine, {})).toBe(mine);
    expect(newestJob(mine, { 4: job({ id: 4, state: 'done' }) })).toBe(mine);
  });
});
