// Chat items folded from bus events. Pure functions.

import { listJobs, newestJob, parseJob, type JobView, type JobsSeen } from './bgjobs';
import type { MessageKey } from './i18n';
import { dateLocale } from './locale.svelte';
import { TASK_TOOLS } from './task';
import { costCard, dayCard, type CostReportData, type DaySummaryData } from './costs';

export interface ChatItem {
  kind: 'user' | 'assistant' | 'tool' | 'error' | 'reminder' | 'timing';
  text: string;
  /** Tool call id for tool rows. */
  id?: string;
  /** Tool name for tool rows. */
  name?: string;
  ok?: boolean;
  ms?: number;
  /** Assistant text still arriving, or tool still running. */
  pending?: boolean;
  /** Card drawn under a finished tool row. */
  data?: CardData;
  /** For a timing row: how long each stage of the reply took, milliseconds. */
  timings?: Timings;
  /** A timing row is drawn only once its reply has ended. */
  done?: boolean;
  /** Share done of a running tool, 0..100, when the tool reports it. */
  progress?: number;
  /** Under a reply: the recording it may save as a skill (decision 0047). */
  offer?: number;
}

/** Stages of a reply: speech to text, first piece of the answer, first sound. */
export interface Timings {
  stt?: number;
  llm?: number;
  tts?: number;
}

const STAGES = ['stt', 'llm', 'tts'] as const;
type Stage = (typeof STAGES)[number];
const isStage = (v: unknown): v is Stage => typeof v === 'string' && (STAGES as readonly string[]).includes(v);

/** Folds the timing rows of the reply that just ended into one row under it. */
function settleTimings(items: ChatItem[]): ChatItem[] {
  const open = items.filter((it) => it.kind === 'timing' && !it.done);
  if (open.length === 0) return items;
  const timings: Timings = Object.assign({}, ...open.map((it) => it.timings));
  return [...items.filter((it) => !(it.kind === 'timing' && !it.done)), { kind: 'timing', text: '', timings, done: true }];
}

/** Open timing rows that belong to a reply that never ended; the trailing ones are for the new message. */
function dropStaleTimings(items: ChatItem[]): ChatItem[] {
  let tail = items.length;
  while (tail > 0 && items[tail - 1].kind === 'timing' && !items[tail - 1].done) tail -= 1;
  return [...items.slice(0, tail).filter((it) => !(it.kind === 'timing' && !it.done)), ...items.slice(tail)];
}

export interface WeatherDay {
  date: string;
  min: number;
  max: number;
  text: string;
}

export interface WeatherData {
  kind: 'weather';
  city: string;
  temp: number;
  feels: number;
  wind: number;
  text: string;
  days: WeatherDay[];
}

export interface VolumeData {
  kind: 'volume';
  level: number;
  was: number;
}

/** A picture the assistant saved: generated or a screenshot. */
export interface ImageData {
  kind: 'image';
  path: string;
}

/** One file a video or sound tool wrote. */
export interface AvFile {
  path: string;
  bytes: number;
  /** Absent for text files: a transcript or subtitles. */
  duration_s?: number;
}

/** Files a video or sound tool wrote. */
export interface AvFilesData {
  kind: 'av_files';
  files: AvFile[];
}

/** A video or sound tool found no FFmpeg: the card offers to download it. */
export interface FfmpegMissingData {
  kind: 'ffmpeg_missing';
}

/** A background job: work that went on after the tool answered, or a screen recording. */
export interface BgJobData {
  kind: 'bg_job';
  job: JobView;
}

/** The report of a deep search. */
export interface ResearchReportData {
  kind: 'research_report';
  path: string;
  title: string;
  sources: number;
  unread: number;
  unsourced: number;
  llm_calls: number;
  tokens: number;
  seconds: number;
  /** Limits that cut the work short. */
  limits: ('time' | 'tokens')[];
}

export type CardData = WeatherData | VolumeData | ImageData | AvFilesData | FfmpegMissingData | CostReportData | DaySummaryData | BgJobData | ResearchReportData;

const num = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

function avFile(v: unknown): AvFile | null {
  const f = obj(v);
  if (!f || typeof f.path !== 'string' || !f.path || !num(f.bytes) || f.bytes < 0) return null;
  return num(f.duration_s) && f.duration_s >= 0 ? { path: f.path, bytes: f.bytes, duration_s: f.duration_s } : { path: f.path, bytes: f.bytes };
}

function isWeatherDay(v: unknown): v is WeatherDay {
  const d = obj(v);
  return !!d && typeof d.date === 'string' && num(d.min) && num(d.max) && typeof d.text === 'string';
}

/** Card data from a tool, or nothing when the shape is not one the chat can draw. */
export function cardData(v: unknown): CardData | undefined {
  const d = obj(v);
  if (!d) return undefined;
  if (d.kind === 'volume' && num(d.level) && num(d.was)) {
    return { kind: 'volume', level: d.level, was: d.was };
  }
  if (d.kind === 'cost_report') return costCard(d);
  if (d.kind === 'day_summary') return dayCard(d);
  if (d.kind === 'image' && typeof d.path === 'string' && d.path) {
    return { kind: 'image', path: d.path };
  }
  if (d.kind === 'av_files' && Array.isArray(d.files)) {
    const files = d.files.map(avFile);
    if (files.length === 0 || files.some((f) => f === null)) return undefined;
    return { kind: 'av_files', files: files as AvFile[] };
  }
  if (d.kind === 'bg_job') {
    const job = parseJob(d.job);
    return job ? { kind: 'bg_job', job } : undefined;
  }
  if (d.kind === 'ffmpeg_missing') {
    return { kind: 'ffmpeg_missing' };
  }
  if (d.kind === 'research_report') {
    const count = (v: unknown): v is number => num(v) && v >= 0;
    if (typeof d.path !== 'string' || !d.path || typeof d.title !== 'string') return undefined;
    if (!count(d.sources) || !count(d.llm_calls) || !count(d.tokens) || !count(d.seconds)) return undefined;
    const limits = Array.isArray(d.limits) ? d.limits.filter((l): l is 'time' | 'tokens' => l === 'time' || l === 'tokens') : [];
    return {
      kind: 'research_report',
      path: d.path,
      title: d.title,
      sources: d.sources,
      unread: count(d.unread) ? d.unread : 0,
      unsourced: count(d.unsourced) ? d.unsourced : 0,
      llm_calls: d.llm_calls,
      tokens: d.tokens,
      seconds: d.seconds,
      limits,
    };
  }
  if (
    d.kind === 'weather' &&
    typeof d.city === 'string' &&
    num(d.temp) &&
    num(d.feels) &&
    num(d.wind) &&
    typeof d.text === 'string' &&
    Array.isArray(d.days) &&
    d.days.every(isWeatherDay)
  ) {
    return { kind: 'weather', city: d.city, temp: d.temp, feels: d.feels, wind: d.wind, text: d.text, days: d.days };
  }
  return undefined;
}

function obj(e: unknown): Record<string, unknown> | null {
  return typeof e === 'object' && e !== null ? (e as Record<string, unknown>) : null;
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null;
}

/** The chat without the chip of an offer: taken, or made old by a new message. */
function dropOffers(items: ChatItem[]): ChatItem[] {
  if (!items.some((it) => it.offer !== undefined)) return items;
  return items.map((it) => {
    if (it.offer === undefined) return it;
    const { offer: _taken, ...rest } = it;
    return rest;
  });
}

/** The chat once the chip of recording `id` was clicked. */
export function takeOffer(items: ChatItem[], id: number): ChatItem[] {
  return items.some((it) => it.offer === id) ? dropOffers(items) : items;
}

function finishStreaming(items: ChatItem[]): ChatItem[] {
  return items.map((it) => (it.kind === 'assistant' && it.pending ? { ...it, pending: false } : it));
}

/** The card of a finished tool: a failed one keeps only the offer to download FFmpeg. */
function finishedCard(ok: boolean, data: unknown): CardData | undefined {
  const card = cardData(data);
  return ok || card?.kind === 'ffmpeg_missing' ? card : undefined;
}

export function reduceChat(items: ChatItem[], event: unknown): ChatItem[] {
  const e = obj(event);
  if (!e) return items;
  switch (e.type) {
    case 'user_text': {
      const text = str(e.text);
      return text === null ? items : [...dropStaleTimings(finishStreaming(dropOffers(items))), { kind: 'user', text }];
    }
    case 'timing': {
      if (!isStage(e.stage) || !num(e.ms)) return items;
      const stage = { [e.stage]: e.ms };
      // Recognition opens a new turn: rows left open by a reply that never ended are dropped.
      if (e.stage === 'stt') {
        return [...items.filter((it) => !(it.kind === 'timing' && !it.done)), { kind: 'timing', text: '', timings: stage }];
      }
      // A row of this reply is already open: the stage joins it where it stands.
      const open = items.findIndex((it) => it.kind === 'timing' && !it.done);
      if (open >= 0) {
        const next = items.slice();
        next[open] = { ...next[open], timings: { ...next[open].timings, ...stage } };
        return next;
      }
      const last = items[items.length - 1];
      // The first sound often comes after the text has ended: it joins the finished row.
      if (e.stage === 'tts' && last?.kind === 'timing') {
        return [...items.slice(0, -1), { ...last, timings: { ...last.timings, ...stage } }];
      }
      if (e.stage === 'tts' && last?.kind === 'assistant' && !last.pending) {
        return [...items, { kind: 'timing', text: '', timings: stage, done: true }];
      }
      // The text still arriving stays last, so the next piece finds it.
      if (last?.kind === 'assistant' && last.pending) {
        return [...items.slice(0, -1), { kind: 'timing', text: '', timings: stage }, last];
      }
      return [...items, { kind: 'timing', text: '', timings: stage }];
    }
    case 'assistant_delta': {
      const text = str(e.text);
      // An empty piece carries only voice marks, for the speaker.
      if (!text) return items;
      const last = items[items.length - 1];
      if (last && last.kind === 'assistant' && last.pending) {
        return [...items.slice(0, -1), { ...last, text: last.text + text }];
      }
      return [...items, { kind: 'assistant', text, pending: true }];
    }
    case 'assistant_text': {
      const text = str(e.text);
      if (text === null) return items;
      const idx = items.findIndex((it) => it.kind === 'assistant' && it.pending);
      if (idx >= 0) return settleTimings(finishStreaming(items));
      return text ? settleTimings([...items, { kind: 'assistant', text }]) : items;
    }
    case 'skill_offer': {
      // The chip goes under the last reply; only one stands at a time.
      if (!num(e.id)) return items;
      let at = items.length - 1;
      while (at >= 0 && items[at].kind !== 'assistant') at -= 1;
      if (at < 0) return items;
      const next = dropOffers(items).slice();
      next[at] = { ...next[at], offer: e.id };
      return next;
    }
    case 'tool_started': {
      const id = str(e.id);
      const name = str(e.name);
      if (id === null || name === null || TASK_TOOLS.includes(name)) return items;
      return [...finishStreaming(items), { kind: 'tool', id, name, text: str(e.summary) ?? name, pending: true }];
    }
    case 'tool_finished': {
      const id = str(e.id);
      const name = str(e.name);
      if (id === null || name === null || TASK_TOOLS.includes(name)) return items;
      const ok = e.ok === true;
      const ms = typeof e.elapsed_ms === 'number' ? e.elapsed_ms : undefined;
      const idx = items.findIndex((it) => it.kind === 'tool' && it.id === id);
      const data = finishedCard(ok, e.data);
      if (idx < 0) {
        return [...items, { kind: 'tool', id, name, text: str(e.output) ?? name, ok, ms, data }];
      }
      const next = items.slice();
      next[idx] = { ...next[idx], ok, ms, pending: false, data, progress: undefined };
      return next;
    }
    case 'tool_progress': {
      const id = str(e.id);
      if (id === null || !num(e.share)) return items;
      const idx = items.findIndex((it) => it.kind === 'tool' && it.id === id && it.pending);
      if (idx < 0) return items;
      const progress = Math.max(0, Math.min(100, Math.round(e.share)));
      if (items[idx].progress === progress) return items;
      const next = items.slice();
      next[idx] = { ...next[idx], progress };
      return next;
    }
    case 'bg_job_updated': {
      // The whole state of a job: the card of that job is redrawn where it stands.
      const job = parseJob(e.job);
      if (!job) return items;
      const data: BgJobData = { kind: 'bg_job', job };
      let found = false;
      const next = items.map((it) => {
        if (it.kind !== 'tool' || it.data?.kind !== 'bg_job' || it.data.job.id !== job.id) return it;
        found = true;
        return { ...it, data };
      });
      return found ? next : items;
    }
    case 'state_changed':
      // A stopped reply never gets its final text: the cursor must not blink on.
      return e.to === 'idle' ? finishStreaming(items) : items;
    case 'error': {
      const message = str(e.message);
      return message === null ? items : [...finishStreaming(items), { kind: 'error', text: message }];
    }
    case 'reminder_fired': {
      const text = str(e.text);
      // A scheduled task shows up as its own user message when it starts.
      if (text === null || e.kind === 'task') return items;
      return [...finishStreaming(items), { kind: 'reminder', text }];
    }
    case 'notification': {
      const app = str(e.app) ?? '';
      const title = str(e.title) ?? '';
      const body = str(e.text) ?? '';
      if (!title && !body) return items;
      const head = [app, title].filter(Boolean).join(' — ');
      return [...finishStreaming(items), { kind: 'reminder', text: body ? `${head}: ${body}` : head }];
    }
    case 'remark': {
      // Her own short line: a break, the end of focus, a meeting soon, a greeting.
      const text = str(e.text)?.trim();
      if (!text) return items;
      return [...finishStreaming(items), { kind: 'reminder', text }];
    }
    default:
      return items;
  }
}

/** Seconds for a tool row: `0,4`, `3,8`, `19`. The unit comes from the locale file. */
export function formatSeconds(ms: number): string {
  if (ms <= 0) return '0';
  const digits = ms < 95 ? 2 : ms < 9950 ? 1 : 0;
  return (ms / 1000).toFixed(digits).replace('.', ',');
}

export interface SpokenWord {
  /** Index of the word from the start of the reply: a stable key, so only new words animate in. */
  i: number;
  word: string;
}

/** Last words of the reply that fit `maxChars`, for the "speaking" island. */
export function speakWords(text: string, maxChars: number): SpokenWord[] {
  const words = text.split(/\s+/).filter(Boolean);
  let from = words.length;
  let size = 0;
  while (from > 0) {
    const next = size + words[from - 1].length + (size ? 1 : 0);
    if (next > maxChars && size > 0) break;
    size = next;
    from -= 1;
  }
  return words.slice(from).map((word, k) => ({ i: from + k, word }));
}

const TOOL_GROUPS: Record<string, MessageKey> = {
  open_app: 'tool.app',
  close_app: 'tool.app',
  list_windows: 'tool.window',
  focus_window: 'tool.window',
  find_files: 'tool.files',
  read_file: 'tool.files',
  write_file: 'tool.files',
  move_path: 'tool.files',
  create_folder: 'tool.files',
  delete_to_trash: 'tool.files',
  open_path: 'tool.files',
  note_create: 'tool.notes',
  note_search: 'tool.notes',
  note_read: 'tool.notes',
  rdp_list: 'tool.rdp',
  rdp_open: 'tool.rdp',
  run_shell: 'tool.shell',
  volume: 'tool.volume',
  app_volume: 'tool.volume',
  system_health: 'tool.system',
  chat_watch: 'tool.message',
  focus_mode: 'tool.todo',
  calendar_events: 'tool.reminder',
  day_summary: 'tool.day',
  cost_report: 'tool.costs',
  screen_time: 'tool.system',
  clipboard_history: 'tool.clipboard',
  docs_search: 'tool.files',
  screen_click: 'tool.screen',
  search_text: 'tool.files',
  disk_usage: 'tool.files',
  power: 'tool.power',
  clipboard: 'tool.clipboard',
  system_info: 'tool.system',
  list_processes: 'tool.system',
  kill_process: 'tool.system',
  open_url: 'tool.web',
  fetch_page: 'tool.web',
  web_search: 'tool.search',
  deep_search: 'tool.search',
  get_weather: 'tool.weather',
  get_time: 'tool.time',
  archive_create: 'tool.files',
  archive_extract: 'tool.files',
  arrange_window: 'tool.window',
  radio: 'tool.device',
  brightness: 'tool.device',
  get_rates: 'tool.rates',
  get_news: 'tool.news',
  reminder_set: 'tool.reminder',
  reminder_list: 'tool.reminder',
  reminder_cancel: 'tool.reminder',
  todo_add: 'tool.todo',
  todo_list: 'tool.todo',
  todo_done: 'tool.todo',
  todo_remove: 'tool.todo',
  memory_save: 'tool.memory',
  memory_search: 'tool.memory',
  memory_forget: 'tool.memory',
  history_search: 'tool.memory',
  media_control: 'tool.media',
  now_playing: 'tool.media',
  messenger_send: 'tool.message',
  ui_read: 'tool.ui',
  ui_click: 'tool.ui',
  ui_type: 'tool.ui',
  ui_keys: 'tool.ui',
  ui_scroll: 'tool.ui',
  screen_look: 'tool.screen',
  screen_text: 'tool.screen',
  screenshot_save: 'tool.screen',
  generate_image: 'tool.image',
  command_save: 'tool.command',
  command_list: 'tool.command',
  command_delete: 'tool.command',
  skill_save: 'tool.skill',
  skill_list: 'tool.skill',
  skill_delete: 'tool.skill',
  skill_run: 'tool.skill',
  trigger_create: 'tool.events',
  trigger_list: 'tool.events',
  trigger_delete: 'tool.events',
  wait_for: 'tool.events',
  wait_list: 'tool.events',
  wait_stop: 'tool.events',
  av_info: 'tool.av',
  av_convert: 'tool.av',
  video_compress: 'tool.av',
  av_cut: 'tool.av',
  av_join: 'tool.av',
  video_gif: 'tool.av',
  video_rotate: 'tool.av',
  av_speed: 'tool.av',
  audio_process: 'tool.av',
  video_subtitles: 'tool.av',
  av_transcribe: 'tool.av',
  video_describe: 'tool.av',
  av_folder: 'tool.av',
  screen_record: 'tool.av',
  screen_record_stop: 'tool.av',
  bg_jobs: 'tool.av',
  bg_job_stop: 'tool.av',
  journal_read: 'tool.journal',
};

/** Short label of the tool group for the pill in the "thinking" island. */
export function toolLabel(name: string): MessageKey {
  if (name.startsWith('mcp_')) return 'tool.extension';
  return Object.prototype.hasOwnProperty.call(TOOL_GROUPS, name) ? TOOL_GROUPS[name] : 'tool.other';
}

/** Megabytes of a file for a card: `48,7`, `245`, `0,3`. The unit comes from the locale file. */
export function megabytes(bytes: number): string {
  const mb = Math.max(0, bytes) / (1024 * 1024);
  const digits = mb >= 100 ? 0 : 1;
  return mb.toFixed(digits).replace('.', ',');
}

/** Length of a recording: `0:07`, `12:31`, `1:02:03`. */
export function clock(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const two = (n: number) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

/** File name from a full Windows path. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Time left until `dueAt` (unix seconds): `9:41`, `1:05:00`, or nothing when it is over a day away. */
export function countdown(dueAt: number, nowMs: number): string {
  const left = Math.max(0, Math.round(dueAt - nowMs / 1000));
  if (left >= 86_400) return '';
  const h = Math.floor(left / 3600);
  const m = Math.floor((left % 3600) / 60);
  const s = left % 60;
  const two = (n: number) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

/** Short weekday of an ISO date, capitalised; empty for a bad date. */
export function weekdayShort(date: string): string {
  const d = new Date(`${date}T12:00:00`);
  if (Number.isNaN(d.getTime())) return '';
  const w = d.toLocaleDateString(dateLocale(), { weekday: 'short' });
  return w.charAt(0).toUpperCase() + w.slice(1);
}

/** Last words of the reply as one string, for the speech bubble. */
export function tailWords(text: string, maxChars: number): string {
  const t = text.replace(/\s+/g, ' ').trim();
  if (t.length <= maxChars) return t;
  const cut = t.slice(t.length - maxChars);
  const space = cut.indexOf(' ');
  return '…' + (space >= 0 ? cut.slice(space + 1) : cut);
}

export function fromHistory(history: unknown): ChatItem[] {
  if (!Array.isArray(history)) return [];
  return history
    .map(obj)
    .filter((h): h is Record<string, unknown> => !!h && (h.role === 'user' || h.role === 'assistant') && typeof h.text === 'string')
    .map((h) => ({ kind: h.role as 'user' | 'assistant', text: h.text as string }));
}

const hasJobCard = (items: ChatItem[], id: number) => items.some((it) => it.data?.kind === 'bg_job' && it.data.job.id === id);

/** One finished tool row with a job card for every job of the list `bg_jobs_list` returns. */
export function fromJobs(jobs: unknown): ChatItem[] {
  return listJobs(jobs).map((job) => ({ kind: 'tool', id: `bgjob-${job.id}`, name: job.kind, text: job.title, ok: true, data: { kind: 'bg_job', job } }));
}

/** The chat with cards for the jobs of the list that have none yet: for a chat drawn anew while jobs run. */
export function addJobs(items: ChatItem[], jobs: unknown): ChatItem[] {
  const rows = fromJobs(jobs).filter((row) => row.data?.kind === 'bg_job' && !hasJobCard(items, row.data.job.id));
  return rows.length > 0 ? [...items, ...rows] : items;
}

/**
 * Job cards take the newer state of their job when an event told about it before
 * the card came: the tool answers with the job as it was when it started.
 */
export function refreshJobs(items: ChatItem[], seen: JobsSeen): ChatItem[] {
  let next: ChatItem[] | null = null;
  items.forEach((it, i) => {
    if (it.data?.kind !== 'bg_job') return;
    const job = newestJob(it.data.job, seen);
    if (job === it.data.job) return;
    next ??= items.slice();
    next[i] = { ...it, data: { kind: 'bg_job', job } };
  });
  return next ?? items;
}

export interface Segment {
  kind: 'text' | 'code' | 'bold';
  text: string;
}

const MARKUP = /`([^`\n]+)`|\*\*([^*\n]+)\*\*/g;

/** Inline code and bold from a model reply; everything else stays plain text (rendered escaped). */
export function inlineMarkup(text: string): Segment[] {
  const out: Segment[] = [];
  let last = 0;
  for (const m of text.matchAll(MARKUP)) {
    const at = m.index ?? 0;
    if (at > last) out.push({ kind: 'text', text: text.slice(last, at) });
    out.push(m[1] !== undefined ? { kind: 'code', text: m[1] } : { kind: 'bold', text: m[2] });
    last = at + m[0].length;
  }
  if (last < text.length || out.length === 0) out.push({ kind: 'text', text: text.slice(last) });
  return out;
}
