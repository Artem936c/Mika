import { describe, expect, it } from 'vitest';
import type { JobView } from './bgjobs';
import {
  addJobs,
  baseName,
  cardData,
  clock,
  countdown,
  formatSeconds,
  fromHistory,
  fromJobs,
  inlineMarkup,
  megabytes,
  reduceChat,
  refreshJobs,
  speakWords,
  tailWords,
  toolLabel,
  weekdayShort,
  type ChatItem,
} from './chat';
import { isMessageKey } from './i18n';

const run = (events: unknown[], start: ChatItem[] = []) => events.reduce<ChatItem[]>(reduceChat, start);

describe('timings', () => {
  const run = (events: unknown[]) => events.reduce<ChatItem[]>((items, e) => reduceChat(items, e), []);

  it('one row under the reply collects every stage', () => {
    const items = run([
      { type: 'timing', stage: 'stt', ms: 900 },
      { type: 'user_text', text: 'привет' },
      { type: 'timing', stage: 'llm', ms: 1200 },
      { type: 'assistant_delta', text: 'Привет!' },
      { type: 'assistant_text', text: 'Привет!' },
      { type: 'timing', stage: 'tts', ms: 1900 },
    ]);
    expect(items.map((it) => it.kind)).toEqual(['user', 'assistant', 'timing']);
    expect(items[2]).toMatchObject({ done: true, timings: { stt: 900, llm: 1200, tts: 1900 } });
  });

  it('the first sound before the end of the text joins the same row', () => {
    const items = run([
      { type: 'user_text', text: 'расскажи' },
      { type: 'timing', stage: 'llm', ms: 800 },
      { type: 'assistant_delta', text: 'Первая фраза. ' },
      { type: 'timing', stage: 'tts', ms: 1500 },
      { type: 'assistant_delta', text: 'Вторая.' },
      { type: 'assistant_text', text: 'Первая фраза. Вторая.' },
    ]);
    expect(items.map((it) => it.kind)).toEqual(['user', 'assistant', 'timing']);
    expect(items[1].text).toBe('Первая фраза. Вторая.');
    expect(items[2].timings).toEqual({ llm: 800, tts: 1500 });
  });

  it('timings of a reply that never ended do not leak into the next one', () => {
    const items = run([
      { type: 'user_text', text: 'раз' },
      { type: 'timing', stage: 'llm', ms: 700 },
      { type: 'error', message: 'обрыв' },
      { type: 'timing', stage: 'stt', ms: 400 },
      { type: 'user_text', text: 'два' },
      { type: 'timing', stage: 'llm', ms: 300 },
      { type: 'assistant_text', text: 'ок' },
    ]);
    const row = items[items.length - 1];
    expect(row.kind).toBe('timing');
    expect(row.timings).toEqual({ stt: 400, llm: 300 });
    expect(items.filter((it) => it.kind === 'timing')).toHaveLength(1);
  });

  it('ignores unknown stages and broken numbers', () => {
    expect(run([{ type: 'timing', stage: 'gpu', ms: 5 }, { type: 'timing', stage: 'llm', ms: 'x' }])).toEqual([]);
  });
});

describe('new events and cards', () => {
  it('a fired reminder becomes its own row, a task does not', () => {
    const items = reduceChat([], { type: 'reminder_fired', id: 1, text: 'чайник', kind: 'remind', late_s: 0 });
    expect(items).toEqual([{ kind: 'reminder', text: 'чайник' }]);
    expect(reduceChat([], { type: 'reminder_fired', id: 2, text: 'сделай', kind: 'task', late_s: 0 })).toEqual([]);
  });

  it('a notification of another program becomes a row', () => {
    const items = reduceChat([], { type: 'notification', app: 'Telegram', title: 'Аня', text: 'привет' });
    expect(items).toEqual([{ kind: 'reminder', text: 'Telegram — Аня: привет' }]);
    expect(reduceChat([], { type: 'notification', app: 'X', title: '', text: '' })).toEqual([]);
  });

  it('her own remark becomes a row', () => {
    expect(reduceChat([], { type: 'remark', text: 'Через 10 мин: Созвон', kind: 'calendar' })).toEqual([
      { kind: 'reminder', text: 'Через 10 мин: Созвон' },
    ]);
    expect(reduceChat([], { type: 'remark', text: ' ', kind: 'life' })).toEqual([]);
  });

  it('image card needs a path', () => {
    expect(cardData({ kind: 'image', path: 'C:\\p\\a.png' })).toEqual({ kind: 'image', path: 'C:\\p\\a.png' });
    expect(cardData({ kind: 'image' })).toBeUndefined();
    expect(cardData({ kind: 'image', path: '' })).toBeUndefined();
  });

  it('labels extension and window tools', () => {
    expect(toolLabel('mcp_files_read')).toBe('tool.extension');
    expect(toolLabel('ui_click')).toBe('tool.ui');
    expect(toolLabel('reminder_set')).toBe('tool.reminder');
    expect(toolLabel('unknown_tool')).toBe('tool.other');
  });

  it('counts down to a reminder', () => {
    const now = 1_000_000_000;
    expect(countdown(1_000_000 + 581, now)).toBe('9:41');
    expect(countdown(1_000_000 + 3900, now)).toBe('1:05:00');
    expect(countdown(1_000_000 - 5, now)).toBe('0:00');
    expect(countdown(1_000_000 + 90_000, now)).toBe('');
  });
});

describe('reduceChat', () => {
  it('a reply stopped midway stops streaming when the state turns idle', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'Шаг 1' },
        { type: 'state_changed', from: 'speaking', to: 'thinking' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Шаг 1', pending: true }]);
    expect(
      run([
        { type: 'assistant_delta', text: 'Шаг 1' },
        { type: 'state_changed', from: 'speaking', to: 'idle' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Шаг 1', pending: false }]);
  });

  it('user_text appends user item', () => {
    expect(run([{ type: 'user_text', text: 'привет' }])).toEqual([{ kind: 'user', text: 'привет' }]);
  });

  it('deltas join into one streaming assistant item', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'При' },
        { type: 'assistant_delta', text: 'вет' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Привет', pending: true }]);
  });

  it('an empty delta never opens a reply of its own', () => {
    const tool: ChatItem = { kind: 'tool', id: 'c1', name: 'x', text: 'x', pending: true };
    expect(run([{ type: 'assistant_delta', text: '' }], [tool])).toEqual([tool]);
    expect(
      run([
        { type: 'assistant_delta', text: 'Удачи!' },
        { type: 'assistant_delta', text: '' },
        { type: 'step_started' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Удачи!', pending: true }]);
  });

  it('assistant_text finalizes the streaming item', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'Ок' },
        { type: 'assistant_text', text: 'Ок' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Ок', pending: false }]);
  });

  it('assistant_text without deltas appends the whole reply', () => {
    expect(run([{ type: 'assistant_text', text: 'Ок' }])).toEqual([{ kind: 'assistant', text: 'Ок' }]);
  });

  it('tool_started then tool_finished updates one tool row with ms', () => {
    expect(
      run([
        { type: 'tool_started', id: 'c1', name: 'get_time', summary: 'Узнать время' },
        { type: 'tool_finished', id: 'c1', name: 'get_time', ok: true, output: '12:00', elapsed_ms: 4 },
      ]),
    ).toEqual([{ kind: 'tool', id: 'c1', name: 'get_time', text: 'Узнать время', ok: true, ms: 4, pending: false }]);
  });

  it('tool_finished without start adds a failed row', () => {
    expect(run([{ type: 'tool_finished', id: 'c9', name: 'x', ok: false, output: 'нет', elapsed_ms: 0 }])).toEqual([
      { kind: 'tool', id: 'c9', name: 'x', text: 'нет', ok: false, ms: 0 },
    ]);
  });

  it('error appends error and finalizes streaming', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'Нача' },
        { type: 'error', message: 'обрыв' },
      ]),
    ).toEqual([
      { kind: 'assistant', text: 'Нача', pending: false },
      { kind: 'error', text: 'обрыв' },
    ]);
  });

  it('malformed events are ignored', () => {
    const start: ChatItem[] = [{ kind: 'user', text: 'a' }];
    expect(run([null, 'x', { type: 'user_text' }, { type: 'assistant_delta', text: 5 }, { type: 'bogus' }], start)).toEqual(start);
  });
});

describe('cardData', () => {
  const weather = {
    kind: 'weather',
    city: 'Пермь',
    temp: 1,
    feels: -3,
    wind: 6,
    text: 'дождь',
    days: [{ date: '2026-11-12', min: -4, max: 0, text: 'снег' }],
  };

  it('accepts weather and volume', () => {
    expect(cardData(weather)).toEqual(weather);
    expect(cardData({ kind: 'volume', level: 40, was: 60 })).toEqual({ kind: 'volume', level: 40, was: 60 });
  });

  it('rejects unknown kinds and broken shapes', () => {
    expect(cardData(undefined)).toBeUndefined();
    expect(cardData({ kind: 'stock', price: 1 })).toBeUndefined();
    expect(cardData({ kind: 'volume', level: '40', was: 60 })).toBeUndefined();
    expect(cardData({ ...weather, temp: null })).toBeUndefined();
    expect(cardData({ ...weather, days: [{ date: 5 }] })).toBeUndefined();
    expect(cardData({ ...weather, days: 'no' })).toBeUndefined();
  });

  it('tool_finished keeps the card data of a successful tool', () => {
    const items = run([
      { type: 'tool_started', id: 'c1', name: 'volume', summary: 'Громкость 40%' },
      { type: 'tool_finished', id: 'c1', name: 'volume', ok: true, output: 'x', elapsed_ms: 5, data: { kind: 'volume', level: 40, was: 60 } },
      { type: 'tool_started', id: 'c2', name: 'volume', summary: 'Громкость 10%' },
      { type: 'tool_finished', id: 'c2', name: 'volume', ok: false, output: 'x', elapsed_ms: 5, data: { kind: 'volume', level: 10, was: 40 } },
      { type: 'tool_started', id: 'c3', name: 'get_time', summary: 'Время' },
      { type: 'tool_finished', id: 'c3', name: 'get_time', ok: true, output: 'x', elapsed_ms: 5, data: { kind: 'bogus' } },
    ]);
    expect(items.map((it) => it.data)).toEqual([{ kind: 'volume', level: 40, was: 60 }, undefined, undefined]);
  });

  it('accepts files of video and sound tools, with or without a length', () => {
    const data = {
      kind: 'av_files',
      files: [
        { path: 'C:\\v\\x (сжато).mp4', bytes: 52428800, duration_s: 75.2 },
        { path: 'C:\\v\\x.srt', bytes: 1200 },
      ],
    };
    expect(cardData(data)).toEqual(data);
    expect(cardData({ kind: 'av_files', files: [{ path: 'C:\\a.mp3', bytes: 10, duration_s: 'long' }] })).toEqual({
      kind: 'av_files',
      files: [{ path: 'C:\\a.mp3', bytes: 10 }],
    });
  });

  it('rejects broken files cards', () => {
    expect(cardData({ kind: 'av_files', files: [] })).toBeUndefined();
    expect(cardData({ kind: 'av_files', files: 'C:\\a.mp4' })).toBeUndefined();
    expect(cardData({ kind: 'av_files', files: [{ path: '', bytes: 1 }] })).toBeUndefined();
    expect(cardData({ kind: 'av_files', files: [{ path: 'C:\\a.mp4' }] })).toBeUndefined();
    expect(cardData({ kind: 'av_files', files: [{ path: 'C:\\a.mp4', bytes: 1 }, null] })).toBeUndefined();
  });

  it('accepts the missing FFmpeg card', () => {
    expect(cardData({ kind: 'ffmpeg_missing' })).toEqual({ kind: 'ffmpeg_missing' });
  });

  it('a failed tool keeps only the missing FFmpeg card', () => {
    const files = { kind: 'av_files', files: [{ path: 'C:\\a.mp4', bytes: 1 }] };
    const items = run([
      { type: 'tool_started', id: 'c1', name: 'video_gif', summary: 'GIF' },
      { type: 'tool_finished', id: 'c1', name: 'video_gif', ok: false, output: 'x', elapsed_ms: 3, data: { kind: 'ffmpeg_missing' } },
      { type: 'tool_started', id: 'c2', name: 'av_cut', summary: 'Cut' },
      { type: 'tool_finished', id: 'c2', name: 'av_cut', ok: false, output: 'x', elapsed_ms: 3, data: files },
      { type: 'tool_finished', id: 'c3', name: 'av_join', ok: false, output: 'x', elapsed_ms: 3, data: { kind: 'ffmpeg_missing' } },
      { type: 'tool_started', id: 'c4', name: 'av_cut', summary: 'Cut' },
      { type: 'tool_finished', id: 'c4', name: 'av_cut', ok: true, output: 'x', elapsed_ms: 3, data: files },
    ]);
    expect(items.map((it) => it.data)).toEqual([{ kind: 'ffmpeg_missing' }, undefined, { kind: 'ffmpeg_missing' }, files]);
  });
});

describe('tool_progress', () => {
  it('sets the share on the running row with that id', () => {
    const items = run([
      { type: 'tool_started', id: 'c1', name: 'video_compress', summary: 'Сжать' },
      { type: 'tool_started', id: 'c2', name: 'av_cut', summary: 'Вырезать' },
      { type: 'tool_progress', id: 'c1', share: 42 },
    ]);
    expect(items.map((it) => it.progress)).toEqual([42, undefined]);
  });

  it('clamps and rounds the share', () => {
    const start = run([{ type: 'tool_started', id: 'c1', name: 'av_speed', summary: 'x' }]);
    expect(run([{ type: 'tool_progress', id: 'c1', share: 140 }], start)[0].progress).toBe(100);
    expect(run([{ type: 'tool_progress', id: 'c1', share: 33.6 }], start)[0].progress).toBe(34);
  });

  it('ignores unknown ids, finished rows and broken shares', () => {
    const start = run([
      { type: 'tool_started', id: 'c1', name: 'av_cut', summary: 'x' },
      { type: 'tool_finished', id: 'c1', name: 'av_cut', ok: true, output: 'x', elapsed_ms: 3 },
      { type: 'tool_started', id: 'c2', name: 'av_cut', summary: 'y' },
    ]);
    expect(run([{ type: 'tool_progress', id: 'c1', share: 50 }], start)).toBe(start);
    expect(run([{ type: 'tool_progress', id: 'c9', share: 50 }], start)).toBe(start);
    expect(run([{ type: 'tool_progress', id: 'c2', share: '50' }], start)).toBe(start);
    expect(run([{ type: 'tool_progress', share: 50 }], start)).toBe(start);
  });

  it('the share goes away when the tool finishes', () => {
    const items = run([
      { type: 'tool_started', id: 'c1', name: 'video_compress', summary: 'x' },
      { type: 'tool_progress', id: 'c1', share: 90 },
      { type: 'tool_finished', id: 'c1', name: 'video_compress', ok: true, output: 'x', elapsed_ms: 3 },
    ]);
    expect(items[0].progress).toBeUndefined();
    expect(items[0].pending).toBe(false);
  });
});

describe('file cards', () => {
  it('formats megabytes, lengths and names', () => {
    expect(megabytes(51_065_000)).toBe('48,7');
    expect(megabytes(256_901_120)).toBe('245');
    expect(megabytes(300_000)).toBe('0,3');
    expect(clock(7.4)).toBe('0:07');
    expect(clock(751.2)).toBe('12:31');
    expect(clock(3723.4)).toBe('1:02:03');
    expect(baseName('C:\\Users\\me\\Videos\\x (сжато).mp4')).toBe('x (сжато).mp4');
    expect(baseName('\\\\server\\share\\a.mp3')).toBe('a.mp3');
  });

  it('labels video and sound tools', () => {
    expect(toolLabel('video_compress')).toBe('tool.av');
    expect(toolLabel('av_transcribe')).toBe('tool.av');
    expect(isMessageKey(toolLabel('av_info'))).toBe(true);
  });

  it('labels folders, recording and background jobs as video and sound', () => {
    for (const name of ['av_folder', 'screen_record', 'screen_record_stop', 'bg_jobs', 'bg_job_stop']) {
      expect(toolLabel(name), name).toBe('tool.av');
    }
  });
});

describe('background jobs', () => {
  const job = (over: Partial<JobView> = {}): JobView => ({
    id: 3,
    kind: 'video_compress',
    title: 'Сжать «отпуск.mp4» до 50 МБ',
    state: 'running',
    live: false,
    share: 0,
    ...over,
  });
  const files = { kind: 'av_files', files: [{ path: 'C:\\v\\отпуск (сжато).mp4', bytes: 51066000, duration_s: 751.2 }] };
  const updated = (j: unknown) => ({ type: 'bg_job_updated', job: j });
  /** A tool call that went to the background: the row and its job card. */
  const started = (id: string, j: JobView) => [
    { type: 'tool_started', id, name: j.kind, summary: j.title },
    { type: 'tool_finished', id, name: j.kind, ok: true, output: 'Запущено в фоне: задание №3', elapsed_ms: 40, data: { kind: 'bg_job', job: j } },
  ];
  const jobOf = (it: ChatItem) => (it.data?.kind === 'bg_job' ? it.data.job : undefined);

  it('cardData reads a job card', () => {
    expect(cardData({ kind: 'bg_job', job: job() })).toEqual({ kind: 'bg_job', job: job() });
    const done = job({ state: 'done', share: 100, text: 'Готово', data: files });
    expect(cardData({ kind: 'bg_job', job: done })).toEqual({ kind: 'bg_job', job: done });
    // The result card inside the job is read by the same function.
    expect(cardData(done.data)).toEqual(files);
  });

  it('cardData refuses a job card of a wrong shape', () => {
    expect(cardData({ kind: 'bg_job' })).toBeUndefined();
    expect(cardData({ kind: 'bg_job', job: null })).toBeUndefined();
    expect(cardData({ kind: 'bg_job', job: { ...job(), state: 'paused' } })).toBeUndefined();
    expect(cardData({ kind: 'bg_job', job: { id: 3 } })).toBeUndefined();
  });

  it('a tool that went to the background gets the job card', () => {
    const items = run(started('c1', job()));
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'tool', id: 'c1', ok: true, pending: false, data: { kind: 'bg_job', job: job() } });
  });

  it('bg_job_updated redraws the card in place', () => {
    const start = run([...started('c1', job()), { type: 'assistant_text', text: 'Запустила в фоне.' }, ...started('c2', job({ id: 4, state: 'queued' }))]);
    const half = run([updated(job({ share: 42, note: '1 из 2: a.mp4' }))], start);
    expect(half.map((it) => it.kind)).toEqual(['tool', 'assistant', 'tool']);
    expect(jobOf(half[0])).toEqual(job({ share: 42, note: '1 из 2: a.mp4' }));
    expect(jobOf(half[2])).toEqual(job({ id: 4, state: 'queued' }));
    // Rows that did not change keep their identity.
    expect(half[1]).toBe(start[1]);
    expect(half[2]).toBe(start[2]);

    const done = run([updated(job({ state: 'done', share: 100, text: 'Готово', data: files }))], half);
    expect(jobOf(done[0])).toEqual(job({ state: 'done', share: 100, text: 'Готово', data: files }));
    expect(done[0]).toMatchObject({ id: 'c1', ok: true, ms: 40 });
  });

  it('bg_job_updated ignores unknown ids and bad shapes', () => {
    const start = run(started('c1', job()));
    expect(run([updated(job({ id: 9, share: 50 }))], start)).toBe(start);
    expect(run([updated({ id: 3, state: 'running' })], start)).toBe(start);
    expect(run([updated(null)], start)).toBe(start);
    expect(run([{ type: 'bg_job_updated' }], start)).toBe(start);
    expect(run([updated(job({ share: 50 }))], [])).toEqual([]);
  });

  it('bg_job_updated never touches a card of another kind', () => {
    const start = run([
      { type: 'tool_started', id: 'c1', name: 'av_cut', summary: 'Вырезать' },
      { type: 'tool_finished', id: 'c1', name: 'av_cut', ok: true, output: 'x', elapsed_ms: 3, data: files },
    ]);
    expect(run([updated(job())], start)).toBe(start);
  });

  it('fromJobs makes one finished tool row per job', () => {
    const rec = job({ id: 5, kind: 'screen_record', title: 'Записать экран', live: true, share: 7, note: '2:15 из 30:00' });
    expect(fromJobs([job({ share: 42 }), rec])).toEqual([
      { kind: 'tool', id: 'bgjob-3', name: 'video_compress', text: 'Сжать «отпуск.mp4» до 50 МБ', ok: true, data: { kind: 'bg_job', job: job({ share: 42 }) } },
      { kind: 'tool', id: 'bgjob-5', name: 'screen_record', text: 'Записать экран', ok: true, data: { kind: 'bg_job', job: rec } },
    ]);
    expect(fromJobs([])).toEqual([]);
    expect(fromJobs(null)).toEqual([]);
    expect(fromJobs([{ id: 1 }, 'x'])).toEqual([]);
  });

  it('rows made by fromJobs follow the events of their jobs', () => {
    const items = run([updated(job({ state: 'stopping', share: 42 }))], fromJobs([job({ share: 42 })]));
    expect(jobOf(items[0])?.state).toBe('stopping');
  });

  it('addJobs adds only jobs that have no card yet', () => {
    const start = run(started('c1', job()));
    expect(addJobs(start, [job({ share: 10 })])).toBe(start);
    const more = addJobs(start, [job({ share: 10 }), job({ id: 4, state: 'queued' })]);
    expect(more.map((it) => it.id)).toEqual(['c1', 'bgjob-4']);
    expect(addJobs(start, 'nothing')).toBe(start);
  });

  it('refreshJobs gives a card the state an event told about before the card came', () => {
    // The job started and even ended before the tool row got its card.
    const done = job({ state: 'done', share: 100, text: 'Готово', data: files });
    const stale = run(started('c1', job({ state: 'queued' })));
    const fresh = refreshJobs(stale, { 3: done });
    expect(jobOf(fresh[0])).toEqual(done);
    expect(fresh[0]).toMatchObject({ id: 'c1', ok: true, ms: 40 });
    // Nothing newer is known: the very same array comes back.
    expect(refreshJobs(fresh, { 3: done })).toBe(fresh);
    expect(refreshJobs(stale, {})).toBe(stale);
    expect(refreshJobs(fresh, { 3: job({ share: 80 }) })).toBe(fresh);
  });
});

describe('formatSeconds', () => {
  it('uses a comma and fewer digits for longer times', () => {
    expect(formatSeconds(400)).toBe('0,4');
    expect(formatSeconds(3849)).toBe('3,8');
    expect(formatSeconds(19090)).toBe('19');
    expect(formatSeconds(27)).toBe('0,03');
    expect(formatSeconds(0)).toBe('0');
  });
});

describe('speakWords', () => {
  it('numbers words from the start of the reply', () => {
    expect(speakWords('  раз два  три ', 100)).toEqual([
      { i: 0, word: 'раз' },
      { i: 1, word: 'два' },
      { i: 2, word: 'три' },
    ]);
  });
  it('keeps only the tail that fits, with the same numbers', () => {
    expect(speakWords('один два три четыре', 10)).toEqual([
      { i: 2, word: 'три' },
      { i: 3, word: 'четыре' },
    ]);
    expect(speakWords('', 10)).toEqual([]);
  });
});

describe('toolLabel', () => {
  it('names a group for known tools and falls back for others', () => {
    expect(toolLabel('get_weather')).toBe('tool.weather');
    expect(toolLabel('run_shell')).toBe('tool.shell');
    expect(toolLabel('brand_new_tool')).toBe('tool.other');
    for (const name of ['open_app', 'find_files', 'note_create', 'rdp_open', 'volume', 'web_search', 'get_time']) {
      expect(isMessageKey(toolLabel(name)), name).toBe(true);
      expect(toolLabel(name)).not.toBe('tool.other');
    }
  });
});

describe('events and watching', () => {
  it('scenario and watch tools share one label', () => {
    for (const name of ['trigger_create', 'trigger_list', 'trigger_delete', 'wait_for', 'wait_list', 'wait_stop']) {
      expect(toolLabel(name), name).toBe('tool.events');
    }
    expect(isMessageKey(toolLabel('wait_for'))).toBe(true);
  });

  it('what she was asked to watch for becomes a row', () => {
    expect(reduceChat([], { type: 'remark', text: 'Процесс cargo.exe завершился.', kind: 'watch' })).toEqual([
      { kind: 'reminder', text: 'Процесс cargo.exe завершился.' },
    ]);
    expect(
      reduceChat([], { type: 'remark', text: 'Сценарий «Вернулся» выключен: команды «сводка» больше нет.', kind: 'trigger' }),
    ).toEqual([{ kind: 'reminder', text: 'Сценарий «Вернулся» выключен: команды «сводка» больше нет.' }]);
  });

  it('a scenario run opens with its own line, like a message', () => {
    const items = run([
      { type: 'user_text', text: '⚡ Сценарий «Наушники»: подключено звуковое устройство' },
      { type: 'tool_started', id: 'c1', name: 'media_control', summary: 'Плеер: продолжить' },
    ]);
    expect(items.map((it) => it.kind)).toEqual(['user', 'tool']);
    expect(items[0].text).toBe('⚡ Сценарий «Наушники»: подключено звуковое устройство');
  });
});

describe('weekdayShort', () => {
  it('gives a capitalised short weekday', () => {
    expect(weekdayShort('2026-11-12')).toBe('Чт');
    expect(weekdayShort('nonsense')).toBe('');
  });
});

describe('tailWords', () => {
  it('keeps short text', () => {
    expect(tailWords(' a  b ', 10)).toBe('a b');
  });
  it('cuts at a word boundary from the end', () => {
    expect(tailWords('один два три четыре', 10)).toBe('…четыре');
  });
});

describe('fromHistory', () => {
  it('maps user and assistant entries and skips junk', () => {
    expect(fromHistory([{ role: 'user', text: 'a' }, { role: 'tool', text: 'x' }, { role: 'assistant', text: 'b' }, 3])).toEqual([
      { kind: 'user', text: 'a' },
      { kind: 'assistant', text: 'b' },
    ]);
    expect(fromHistory(null)).toEqual([]);
  });
});

describe('inlineMarkup', () => {
  it('plain text is one text segment', () => {
    expect(inlineMarkup('Привет')).toEqual([{ kind: 'text', text: 'Привет' }]);
  });

  it('splits inline code and bold out of the text', () => {
    expect(inlineMarkup('Создала `text.txt` в **Загрузках**.')).toEqual([
      { kind: 'text', text: 'Создала ' },
      { kind: 'code', text: 'text.txt' },
      { kind: 'text', text: ' в ' },
      { kind: 'bold', text: 'Загрузках' },
      { kind: 'text', text: '.' },
    ]);
  });

  it('leaves an unclosed marker as text while the reply streams', () => {
    expect(inlineMarkup('файл `text.t')).toEqual([{ kind: 'text', text: 'файл `text.t' }]);
  });
});

describe('research report card', () => {
  const report = {
    kind: 'research_report',
    path: 'C:\\Users\\me\\Documents\\Mika\\Research\\2026-11-14 12-30 Чем тепловой насос.md',
    title: 'Чем тепловой насос «воздух-вода» отличается от «грунт-вода»',
    sources: 6,
    unread: 3,
    unsourced: 1,
    llm_calls: 9,
    tokens: 38200,
    seconds: 143,
    limits: [],
  };

  it('accepts the report of a deep search', () => {
    expect(cardData(report)).toEqual(report);
    expect(cardData({ ...report, limits: ['time', 'tokens'], extra: 'x' })).toEqual({ ...report, limits: ['time', 'tokens'] });
  });

  it('fills what an older core does not send', () => {
    const { unread: _unread, unsourced: _unsourced, limits: _limits, ...bare } = report;
    expect(cardData(bare)).toEqual({ ...report, unread: 0, unsourced: 0, limits: [] });
    expect(cardData({ ...report, unread: -1, unsourced: 'many', limits: 'time' })).toEqual({ ...report, unread: 0, unsourced: 0, limits: [] });
  });

  it('drops limits it does not know', () => {
    expect(cardData({ ...report, limits: ['time', 'money', 5, null, 'tokens'] })).toEqual({ ...report, limits: ['time', 'tokens'] });
  });

  it('rejects broken shapes', () => {
    expect(cardData({ ...report, path: '' })).toBeUndefined();
    expect(cardData({ ...report, path: 5 })).toBeUndefined();
    expect(cardData({ ...report, title: null })).toBeUndefined();
    expect(cardData({ ...report, sources: '6' })).toBeUndefined();
    expect(cardData({ ...report, sources: -1 })).toBeUndefined();
    expect(cardData({ ...report, llm_calls: Number.NaN })).toBeUndefined();
    expect(cardData({ ...report, tokens: undefined })).toBeUndefined();
    expect(cardData({ ...report, seconds: Number.POSITIVE_INFINITY })).toBeUndefined();
    expect(cardData({ kind: 'research_report' })).toBeUndefined();
  });

  it('a failed deep search shows no card', () => {
    const items = run([
      { type: 'tool_started', id: 'r1', name: 'deep_search', summary: 'Глубокий поиск' },
      { type: 'tool_finished', id: 'r1', name: 'deep_search', ok: false, output: 'Ошибка', elapsed_ms: 9100, data: report },
      { type: 'tool_started', id: 'r2', name: 'deep_search', summary: 'Глубокий поиск' },
      { type: 'tool_progress', id: 'r2', share: 62 },
      { type: 'tool_finished', id: 'r2', name: 'deep_search', ok: true, output: '', elapsed_ms: 143000, data: report },
    ]);
    expect(items.map((it) => it.data)).toEqual([undefined, report]);
    expect(items[1].progress).toBeUndefined();
  });

  it('labels the tool as a search', () => {
    expect(toolLabel('deep_search')).toBe('tool.search');
    expect(isMessageKey(toolLabel('deep_search'))).toBe(true);
  });
});
