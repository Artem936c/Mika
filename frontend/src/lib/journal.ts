// The journal page of the settings: what the journal commands send and take, the filter and
// its query, live rows merged into the list, the texts of a row. Pure functions.

import { dateLocale, isMessageKey, t } from './i18n';

export const LEVELS = ['error', 'warn', 'info', 'debug'] as const;
export type Level = (typeof LEVELS)[number];

/** The categories of the journal, in the order the filter lists them. */
export const CATEGORIES = [
  'app',
  'turn',
  'llm',
  'tool',
  'policy',
  'voice',
  'telegram',
  'mcp',
  'events',
  'jobs',
  'costs',
  'backup',
  'update',
  'hotkeys',
  'web',
  'lib',
] as const;

/** Rows asked for at once. */
export const PAGE_SIZE = 100;
/** The most `journal_read` gives in one answer. */
export const MAX_LIMIT = 500;
/** The most rows the page draws, however long it is followed. */
export const MAX_ROWS = 1000;
/** How often new rows are asked for while «follow» is on. */
export const TAIL_MS = 2000;
/** The pause after the last typed character before the search runs. */
export const SEARCH_MS = 300;
/** How long the clear button waits for its second press. */
export const SURE_MS = 4000;
/** A message longer than this is folded. */
export const FOLD_AT = 200;
export const MIN_KEEP_DAYS = 1;
export const MAX_KEEP_DAYS = 365;

export interface JournalRow {
  /** Number of the row among those kept in memory; `null` for a row read from a file only. */
  seq: number | null;
  /** RFC 3339 with milliseconds and the local offset. */
  t: string;
  level: Level;
  category: string;
  message: string;
}

export interface JournalSettings {
  level: Level;
  content: boolean;
  keep_days: number;
  keep_files: number;
}

export interface JournalView extends JournalSettings {
  dir: string;
  size_bytes: number;
  files: number;
  /** Time of the first row, `null` for an empty journal. */
  oldest: string | null;
  /** The previous run did not end cleanly and nobody has been told yet. */
  crashed: boolean;
}

export interface JournalQuery {
  /** Empty: every level. */
  levels: string[];
  /** Empty: every category. */
  categories: string[];
  text: string;
  /** The mark of the page to read, `null` for the newest rows. */
  before: string | null;
  limit: number;
}

/** What the page filters by. */
export interface JournalFilter {
  levels: Level[];
  categories: string[];
  text: string;
}

export interface JournalPage {
  /** Newest first. */
  rows: JournalRow[];
  next: string | null;
}

export interface JournalTail {
  /** Oldest first. */
  rows: JournalRow[];
  last: number | null;
}

const isObject = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null;
const isLevel = (value: unknown): value is Level => LEVELS.includes(value as Level);

/** Rows of an answer; what is not a row is dropped, a level this build does not know reads as `info`. */
export function rowsOf(value: unknown): JournalRow[] {
  if (!Array.isArray(value)) return [];
  const rows: JournalRow[] = [];
  for (const item of value) {
    if (!isObject(item) || typeof item.message !== 'string') continue;
    const level = typeof item.level === 'string' ? item.level.toLowerCase() : '';
    rows.push({
      seq: typeof item.seq === 'number' && Number.isFinite(item.seq) ? item.seq : null,
      t: typeof item.t === 'string' ? item.t : '',
      level: isLevel(level) ? level : 'info',
      category: typeof item.category === 'string' ? item.category : '',
      message: item.message,
    });
  }
  return rows;
}

/** The answer of `journal_read`. */
export function pageOf(value: unknown): JournalPage {
  if (!isObject(value)) return { rows: [], next: null };
  return { rows: rowsOf(value.rows), next: typeof value.next === 'string' && value.next !== '' ? value.next : null };
}

/** The answer of `journal_tail`. */
export function tailOf(value: unknown): JournalTail {
  if (!isObject(value)) return { rows: [], last: null };
  return { rows: rowsOf(value.rows), last: typeof value.last === 'number' && Number.isFinite(value.last) ? value.last : null };
}

/** Whether the app says that its previous run was cut short. */
export function crashedOf(view: unknown): boolean {
  return isObject(view) && view.crashed === true;
}

export function emptyFilter(): JournalFilter {
  return { levels: [], categories: [], text: '' };
}

/** The filter with `level` switched; the levels keep the order of `LEVELS`. */
export function toggleLevel(filter: JournalFilter, level: Level): JournalFilter {
  const on = filter.levels.includes(level);
  return { ...filter, levels: LEVELS.filter((l) => (l === level ? !on : filter.levels.includes(l))) };
}

/** The filter narrowed to one category; an empty name is every category. */
export function withCategory(filter: JournalFilter, category: string): JournalFilter {
  return { ...filter, categories: category ? [category] : [] };
}

export function withText(filter: JournalFilter, text: string): JournalFilter {
  return { ...filter, text };
}

/** Whether the filter hides anything. */
export function filterActive(filter: JournalFilter): boolean {
  return filter.levels.length > 0 || filter.categories.length > 0 || filter.text.trim() !== '';
}

export function sameFilter(a: JournalFilter, b: JournalFilter): boolean {
  return a.text.trim() === b.text.trim() && a.levels.join() === b.levels.join() && a.categories.join() === b.categories.join();
}

/** The query of `journal_read` for one page of the filter. */
export function buildQuery(filter: JournalFilter, before: string | null = null, limit = PAGE_SIZE): JournalQuery {
  const count = Number.isFinite(limit) ? Math.round(limit) : PAGE_SIZE;
  return {
    levels: [...filter.levels],
    categories: [...filter.categories],
    text: filter.text.trim(),
    before,
    limit: Math.min(MAX_LIMIT, Math.max(1, count)),
  };
}

/** What `journal_export` takes: the filter when it hides anything, else the whole journal. */
export function exportQuery(filter: JournalFilter): JournalQuery | null {
  return filterActive(filter) ? buildQuery(filter, null, MAX_LIMIT) : null;
}

/** Whether a live row belongs to the filtered list. The text is looked for in the message, in any case. */
export function matches(row: JournalRow, filter: JournalFilter): boolean {
  if (filter.levels.length > 0 && !filter.levels.includes(row.level)) return false;
  if (filter.categories.length > 0 && !filter.categories.includes(row.category)) return false;
  const text = filter.text.trim().toLowerCase();
  return text === '' || row.message.toLowerCase().includes(text);
}

/** What tells two rows without a number apart. */
function contentKey(row: JournalRow): string {
  return `${row.t}\u0000${row.level}\u0000${row.category}\u0000${row.message}`;
}

function maxSeq(rows: JournalRow[]): number | null {
  let max: number | null = null;
  for (const row of rows) if (row.seq !== null && (max === null || row.seq > max)) max = row.seq;
  return max;
}

/**
 * The list (newest first) with the live rows of `journal_tail` (oldest first) put on top.
 * A row the list already has is not added twice: by its number when both sides have numbers,
 * else by its time and text; a row older than the top of the list belongs to a later page.
 * A row the list holds without a number (read from the file a moment before its number was known)
 * is told by its time and text too, whatever numbers stand beside it.
 * The list is cut at `cap` from its old end. Nothing new: the same array comes back.
 */
export function mergeTail(shown: JournalRow[], fresh: JournalRow[], filter: JournalFilter, cap = MAX_ROWS): JournalRow[] {
  const top = maxSeq(shown);
  const newest = shown.length > 0 ? Date.parse(shown[0].t) : Number.NEGATIVE_INFINITY;
  const known = new Set(shown.map(contentKey));
  const unnumbered = new Set(shown.filter((row) => row.seq === null).map(contentKey));
  const added: JournalRow[] = [];
  for (const row of fresh) {
    if (!matches(row, filter)) continue;
    if (row.seq !== null && top !== null) {
      if (row.seq <= top) continue;
      if (unnumbered.has(contentKey(row))) continue;
    } else {
      if (known.has(contentKey(row))) continue;
      if (Date.parse(row.t) < newest) continue;
    }
    added.push(row);
  }
  if (added.length === 0) return shown;
  return [...added.reverse(), ...shown].slice(0, cap);
}

/**
 * Whether the typed filter still has to be read. `pending` is the filter of the read that is on
 * its way, if any: compared with the filter of the last read that landed, a text typed and
 * cleared while a slow read runs would look "already applied", and the list would stay filtered
 * by a text the box no longer shows.
 */
export function needsReload(filter: JournalFilter, applied: JournalFilter, pending: JournalFilter | null): boolean {
  return !sameFilter(filter, pending ?? applied);
}

/** The list with the next page under it, cut at `cap`. */
export function appendPage(shown: JournalRow[], page: JournalRow[], cap = MAX_ROWS): JournalRow[] {
  const seqs = new Set(shown.filter((row) => row.seq !== null).map((row) => row.seq));
  const known = new Set(shown.filter((row) => row.seq === null).map(contentKey));
  const added = page.filter((row) => (row.seq !== null ? !seqs.has(row.seq) : !known.has(contentKey(row))));
  return [...shown, ...added].slice(0, cap);
}

/** The number `journal_tail` is asked after: it only grows. */
export function tailCursor(known: number | null, rows: JournalRow[], last: number | null = null): number | null {
  const seen = [known, last, maxSeq(rows)].filter((n): n is number => n !== null);
  return seen.length > 0 ? Math.max(...seen) : null;
}

/** Whether «show more» has a page to bring and room to draw it. */
export function canShowMore(count: number, next: string | null, cap = MAX_ROWS): boolean {
  return next !== null && count < cap;
}

/** A key for every row of the list, unique within it: rows that look the same are numbered. */
export function rowKeys(rows: JournalRow[]): string[] {
  const seen = new Map<string, number>();
  return rows.map((row) => {
    if (row.seq !== null) return `#${row.seq}`;
    const key = contentKey(row);
    const n = seen.get(key) ?? 0;
    seen.set(key, n + 1);
    return `${key}\u0000${n}`;
  });
}

const STAMP = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}:\d{2}:\d{2})/;
const two = (n: number) => String(n).padStart(2, '0');

/** The day of `now` as the journal writes days: `2026-11-14`, local time. */
export function dayKey(now: Date): string {
  return `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
}

/**
 * The time of a row as it was written: `15:30:05` for a row of `today` (see `dayKey`),
 * `13.11 15:30:05` for another day, with the year for another year. Not a time: shown as it is.
 */
export function rowTime(stamp: string, today: string): string {
  const m = STAMP.exec(stamp);
  if (!m) return stamp;
  const [, year, month, day, time] = m;
  if (`${year}-${month}-${day}` === today) return time;
  return year === today.slice(0, 4) ? `${day}.${month} ${time}` : `${day}.${month}.${year} ${time}`;
}

/** The name of a category; one this build does not know shows its id. */
export function categoryLabel(category: string): string {
  const key = `journal.cat.${category}`;
  return isMessageKey(key) ? t(key) : category;
}

/** Whether the message is shown folded first. */
export function isLong(message: string): boolean {
  return message.length > FOLD_AT || message.includes('\n');
}

/** The start of a long message: its first line, cut at `FOLD_AT` characters. */
export function folded(message: string): string {
  const line = message.split('\n', 1)[0].trimEnd();
  const chars = Array.from(line);
  return `${chars.length > FOLD_AT ? chars.slice(0, FOLD_AT).join('').trimEnd() : line}…`;
}

/** A row as text to copy: the fields of the file, in its order. */
export function rowLine(row: JournalRow): string {
  return `${row.t} ${row.level.toUpperCase()} ${row.category} ${row.message}`;
}

/** A size on disk: bytes, then whole kilobytes, then megabytes with one decimal. */
export function sizeText(bytes: number): string {
  const n = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  const number = (value: number, digits: number) => new Intl.NumberFormat(dateLocale(), { maximumFractionDigits: digits }).format(value);
  if (n < 1024) return t('journal.size_b', { value: number(n, 0) });
  if (n < 1024 * 1024) return t('journal.size_kb', { value: number(n / 1024, 0) });
  return t('journal.size_mb', { value: number(n / (1024 * 1024), 1) });
}

/** The day of the first row in words, day and month. Not a time: an empty string. */
export function sinceText(oldest: string | null): string {
  const m = oldest ? STAMP.exec(oldest) : null;
  if (!m) return '';
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).toLocaleDateString(dateLocale(), { day: 'numeric', month: 'long' });
}

/** How much the journal takes, in how many files, since which day. */
export function sizeLine(view: Pick<JournalView, 'size_bytes' | 'files' | 'oldest'>): string {
  const size = sizeText(view.size_bytes);
  const since = sinceText(view.oldest);
  return since ? t('journal.size', { size, files: view.files, since }) : t('journal.size_undated', { size, files: view.files });
}

/** The settings `set_journal` takes, as the view holds them. */
export function settingsOf(view: JournalSettings): JournalSettings {
  return { level: view.level, content: view.content, keep_days: view.keep_days, keep_files: view.keep_files };
}

/** Days of keeping typed into the field: a whole number within the limits, else `fallback`. */
export function cleanDays(value: unknown, fallback: number): number {
  const typed = typeof value === 'string' ? value.trim() : value;
  const days = typed === '' || typed === null ? Number.NaN : Number(typed);
  if (!Number.isFinite(days)) return fallback;
  return Math.min(MAX_KEEP_DAYS, Math.max(MIN_KEEP_DAYS, Math.round(days)));
}
