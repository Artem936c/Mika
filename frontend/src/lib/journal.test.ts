import { afterEach, describe, expect, it } from 'vitest';
import { isMessageKey } from './i18n';
import {
  CATEGORIES,
  FOLD_AT,
  LEVELS,
  MAX_LIMIT,
  MAX_ROWS,
  PAGE_SIZE,
  appendPage,
  buildQuery,
  canShowMore,
  categoryLabel,
  cleanDays,
  crashedOf,
  dayKey,
  emptyFilter,
  exportQuery,
  filterActive,
  folded,
  isLong,
  matches,
  mergeTail,
  needsReload,
  pageOf,
  rowKeys,
  rowLine,
  rowTime,
  rowsOf,
  sameFilter,
  settingsOf,
  sinceText,
  sizeLine,
  sizeText,
  tailCursor,
  tailOf,
  toggleLevel,
  withCategory,
  withText,
  type JournalFilter,
  type JournalRow,
  type JournalView,
} from './journal';
import { setLang } from './locale.svelte';

/** A row written `second` seconds after 15:30:00. */
function row(seq: number | null, second: number, extra: Partial<JournalRow> = {}): JournalRow {
  return {
    seq,
    t: `2026-11-14T15:30:${String(second).padStart(2, '0')}.000+05:00`,
    level: 'info',
    category: 'tool',
    message: `row ${second}`,
    ...extra,
  };
}

const VIEW: JournalView = {
  level: 'info',
  content: false,
  keep_days: 14,
  keep_files: 5,
  dir: String.raw`C:\Users\me\AppData\Local\Mika\logs`,
  size_bytes: 3_565_158,
  files: 3,
  oldest: '2026-11-01T09:12:44.120+05:00',
  crashed: false,
};

afterEach(() => setLang('ru'));

describe('answers of the commands', () => {
  it('keeps the rows and drops what is not a row', () => {
    const rows = rowsOf([
      { seq: 7, t: '2026-11-14T15:30:05.123+05:00', level: 'error', category: 'tool', message: 'failed' },
      { seq: null, t: '2026-11-14T15:30:04.000+05:00', level: 'WARN', category: 'llm', message: 'slow' },
      { t: '2026-11-14T15:30:03.000+05:00', level: 'trace', category: 'lib', message: 'chatty' },
      { seq: 1, level: 'info' },
      null,
      'text',
    ]);
    expect(rows).toEqual([
      { seq: 7, t: '2026-11-14T15:30:05.123+05:00', level: 'error', category: 'tool', message: 'failed' },
      { seq: null, t: '2026-11-14T15:30:04.000+05:00', level: 'warn', category: 'llm', message: 'slow' },
      { seq: null, t: '2026-11-14T15:30:03.000+05:00', level: 'info', category: 'lib', message: 'chatty' },
    ]);
    expect(rowsOf(null)).toEqual([]);
    expect(rowsOf({ rows: [] })).toEqual([]);
  });

  it('reads a page and a tail, also from an answer that is not one', () => {
    expect(pageOf({ rows: [row(1, 1)], next: 'abc' })).toEqual({ rows: [row(1, 1)], next: 'abc' });
    expect(pageOf({ rows: [], next: null })).toEqual({ rows: [], next: null });
    expect(pageOf({ rows: [], next: '' }).next).toBeNull();
    expect(pageOf(null)).toEqual({ rows: [], next: null });
    expect(tailOf({ rows: [row(4, 4)], last: 4 })).toEqual({ rows: [row(4, 4)], last: 4 });
    expect(tailOf({ rows: [], last: null })).toEqual({ rows: [], last: null });
    expect(tailOf(undefined)).toEqual({ rows: [], last: null });
  });

  it('knows a run that was cut short', () => {
    expect(crashedOf({ ...VIEW, crashed: true })).toBe(true);
    expect(crashedOf(VIEW)).toBe(false);
    expect(crashedOf(null)).toBe(false);
    expect(crashedOf({ crashed: 'yes' })).toBe(false);
  });
});

describe('filter', () => {
  it('starts with nothing hidden', () => {
    expect(emptyFilter()).toEqual({ levels: [], categories: [], text: '' });
    expect(filterActive(emptyFilter())).toBe(false);
    expect(filterActive(withText(emptyFilter(), '   '))).toBe(false);
  });

  it('switches a level and keeps the order of the levels', () => {
    let f = toggleLevel(emptyFilter(), 'warn');
    f = toggleLevel(f, 'error');
    expect(f.levels).toEqual(['error', 'warn']);
    expect(filterActive(f)).toBe(true);
    f = toggleLevel(f, 'warn');
    expect(f.levels).toEqual(['error']);
    expect(toggleLevel(f, 'error').levels).toEqual([]);
  });

  it('narrows to one category and back to all', () => {
    const f = withCategory(emptyFilter(), 'tool');
    expect(f.categories).toEqual(['tool']);
    expect(withCategory(f, 'llm').categories).toEqual(['llm']);
    expect(withCategory(f, '').categories).toEqual([]);
  });

  it('does not change the filter it was given', () => {
    const f = emptyFilter();
    toggleLevel(f, 'error');
    withCategory(f, 'tool');
    withText(f, 'x');
    expect(f).toEqual(emptyFilter());
  });

  it('compares filters by what they ask for', () => {
    const f = withText(toggleLevel(emptyFilter(), 'error'), 'timeout');
    expect(sameFilter(f, { levels: ['error'], categories: [], text: ' timeout ' })).toBe(true);
    expect(sameFilter(f, withText(f, 'time'))).toBe(false);
    expect(sameFilter(f, withCategory(f, 'tool'))).toBe(false);
    expect(sameFilter(f, toggleLevel(f, 'warn'))).toBe(false);
  });
});

describe('query', () => {
  it('asks for the first page of everything', () => {
    expect(buildQuery(emptyFilter())).toEqual({ levels: [], categories: [], text: '', before: null, limit: PAGE_SIZE });
  });

  it('carries the filter, the trimmed text and the page mark', () => {
    const f: JournalFilter = { levels: ['error', 'warn'], categories: ['tool'], text: '  timeout ' };
    expect(buildQuery(f, 'mark-2')).toEqual({ levels: ['error', 'warn'], categories: ['tool'], text: 'timeout', before: 'mark-2', limit: PAGE_SIZE });
  });

  it('keeps the limit within what the command takes', () => {
    expect(buildQuery(emptyFilter(), null, 5000).limit).toBe(MAX_LIMIT);
    expect(buildQuery(emptyFilter(), null, 0).limit).toBe(1);
    expect(buildQuery(emptyFilter(), null, Number.NaN).limit).toBe(PAGE_SIZE);
    expect(MAX_LIMIT).toBe(500);
  });

  it('does not share its lists with the filter', () => {
    const f = toggleLevel(emptyFilter(), 'error');
    const q = buildQuery(f);
    q.levels.push('debug');
    expect(f.levels).toEqual(['error']);
  });

  it('exports the whole journal unless something is filtered', () => {
    expect(exportQuery(emptyFilter())).toBeNull();
    expect(exportQuery(withText(emptyFilter(), ' '))).toBeNull();
    expect(exportQuery(withCategory(emptyFilter(), 'llm'))).toEqual({ levels: [], categories: ['llm'], text: '', before: null, limit: MAX_LIMIT });
  });
});

describe('matches', () => {
  const r = row(1, 1, { level: 'error', category: 'tool', message: 'run_shell failed: Timeout after 30 s' });

  it('takes every row without a filter', () => {
    expect(matches(r, emptyFilter())).toBe(true);
  });

  it('checks the level, the category and the text', () => {
    expect(matches(r, { levels: ['error', 'warn'], categories: [], text: '' })).toBe(true);
    expect(matches(r, { levels: ['info'], categories: [], text: '' })).toBe(false);
    expect(matches(r, { levels: [], categories: ['tool'], text: '' })).toBe(true);
    expect(matches(r, { levels: [], categories: ['llm'], text: '' })).toBe(false);
    expect(matches(r, { levels: [], categories: [], text: ' timeout ' })).toBe(true);
    expect(matches(r, { levels: [], categories: [], text: 'refused' })).toBe(false);
    expect(matches(r, { levels: ['error'], categories: ['tool'], text: 'SHELL' })).toBe(true);
  });
});

describe('mergeTail', () => {
  const shown = [row(3, 3), row(2, 2), row(1, 1)];

  it('puts new rows on top, newest first', () => {
    const merged = mergeTail(shown, [row(4, 4), row(5, 5)], emptyFilter());
    expect(merged.map((r) => r.seq)).toEqual([5, 4, 3, 2, 1]);
  });

  it('adds no row twice by its number', () => {
    const merged = mergeTail(shown, [row(2, 2), row(3, 3), row(4, 4)], emptyFilter());
    expect(merged.map((r) => r.seq)).toEqual([4, 3, 2, 1]);
  });

  it('gives the same array back when nothing is new', () => {
    expect(mergeTail(shown, [], emptyFilter())).toBe(shown);
    expect(mergeTail(shown, [row(1, 1), row(3, 3)], emptyFilter())).toBe(shown);
  });

  it('leaves out live rows the filter hides', () => {
    const errors: JournalFilter = { levels: ['error'], categories: [], text: '' };
    const list = [row(3, 3, { level: 'error' })];
    const merged = mergeTail(list, [row(4, 4), row(5, 5, { level: 'error' }), row(6, 6, { level: 'warn' })], errors);
    expect(merged.map((r) => r.seq)).toEqual([5, 3]);
  });

  it('knows the rows read from the files by their time and text', () => {
    // The list came from the files without numbers; the first tail brings all that is in memory.
    const files = [row(null, 3), row(null, 2), row(null, 1)];
    const merged = mergeTail(files, [row(11, 1), row(12, 2), row(13, 3), row(14, 4)], emptyFilter());
    expect(merged.map((r) => r.message)).toEqual(['row 4', 'row 3', 'row 2', 'row 1']);
    expect(merged[0].seq).toBe(14);
  });

  it('does not pull in rows older than the top of the list', () => {
    // Only errors are shown, the newest is from second 9. Memory also holds an error of second 5
    // that lies on a later page: «show more» brings it, the tail must not.
    const errors: JournalFilter = { levels: ['error'], categories: [], text: '' };
    const list = [row(null, 9, { level: 'error' })];
    const merged = mergeTail(list, [row(20, 5, { level: 'error' }), row(21, 9, { level: 'error' }), row(22, 10, { level: 'error' })], errors);
    expect(merged.map((r) => r.message)).toEqual(['row 10', 'row 9']);
  });

  it('knows a row without a number among numbered ones', () => {
    // The newest row was read from the file a moment before its number was known; the tail
    // brings the same record with its number, above the top of the list.
    const list = [row(null, 5), row(4, 4), row(3, 3)];
    const merged = mergeTail(list, [row(5, 5), row(6, 6)], emptyFilter());
    expect(merged.map((r) => r.message)).toEqual(['row 6', 'row 5', 'row 4', 'row 3']);
    expect(mergeTail(list, [row(5, 5)], emptyFilter())).toBe(list);
  });

  it('keeps two different rows of the same millisecond', () => {
    const list = [row(null, 3, { message: 'first' })];
    const merged = mergeTail(list, [row(8, 3, { message: 'second' })], emptyFilter());
    expect(merged.map((r) => r.message)).toEqual(['second', 'first']);
  });

  it('fills an empty list', () => {
    expect(mergeTail([], [row(1, 1), row(2, 2)], emptyFilter()).map((r) => r.seq)).toEqual([2, 1]);
  });

  it('never grows past the cap: the oldest rows go', () => {
    const merged = mergeTail(shown, [row(4, 4), row(5, 5)], emptyFilter(), 4);
    expect(merged.map((r) => r.seq)).toEqual([5, 4, 3, 2]);
    let list: JournalRow[] = [];
    for (let i = 1; i <= MAX_ROWS + 50; i += 1) list = mergeTail(list, [row(i, i % 60, { message: `m${i}` })], emptyFilter());
    expect(list.length).toBe(MAX_ROWS);
    expect(list[0].seq).toBe(MAX_ROWS + 50);
    expect(list[list.length - 1].seq).toBe(51);
  });

  it('does not change the list it was given', () => {
    const before = [...shown];
    mergeTail(shown, [row(4, 4)], emptyFilter());
    expect(shown).toEqual(before);
  });
});

describe('needsReload', () => {
  const none = emptyFilter();
  const typed = withText(none, 'ab');

  it('reads a filter that differs from the one shown', () => {
    expect(needsReload(typed, none, null)).toBe(true);
    expect(needsReload(none, none, null)).toBe(false);
  });

  it('compares with the read on its way, not with the one that landed', () => {
    // 'ab' is being read; the box was cleared meanwhile: the empty filter must be read again.
    expect(needsReload(none, none, typed)).toBe(true);
    // The same text is already on its way: no second read.
    expect(needsReload(typed, none, typed)).toBe(false);
  });
});

describe('appendPage', () => {
  it('puts the next page under the list', () => {
    const merged = appendPage([row(4, 4), row(3, 3)], [row(2, 2), row(null, 1)]);
    expect(merged.map((r) => r.message)).toEqual(['row 4', 'row 3', 'row 2', 'row 1']);
  });

  it('adds no row twice', () => {
    const merged = appendPage([row(4, 4), row(null, 3)], [row(4, 4), row(null, 3), row(null, 2)]);
    expect(merged.map((r) => r.message)).toEqual(['row 4', 'row 3', 'row 2']);
  });

  it('stops at the cap', () => {
    expect(appendPage([row(4, 4), row(3, 3)], [row(2, 2), row(1, 1)], 3).map((r) => r.seq)).toEqual([4, 3, 2]);
  });
});

describe('cursor', () => {
  it('only grows', () => {
    expect(tailCursor(null, [])).toBeNull();
    expect(tailCursor(null, [row(null, 1)])).toBeNull();
    expect(tailCursor(null, [row(5, 5), row(4, 4)])).toBe(5);
    expect(tailCursor(7, [row(5, 5)])).toBe(7);
    expect(tailCursor(7, [], 9)).toBe(9);
    expect(tailCursor(7, [row(12, 1)], 9)).toBe(12);
    expect(tailCursor(7, [], null)).toBe(7);
  });

  it('offers more only with a page mark and room to draw', () => {
    expect(canShowMore(100, 'mark')).toBe(true);
    expect(canShowMore(100, null)).toBe(false);
    expect(canShowMore(MAX_ROWS, 'mark')).toBe(false);
    expect(canShowMore(3, 'mark', 3)).toBe(false);
  });
});

describe('row keys', () => {
  it('are unique, also for rows that look the same', () => {
    const rows = [row(9, 9), row(null, 3), row(null, 3), row(null, 3, { message: 'other' }), row(8, 3)];
    const keys = rowKeys(rows);
    expect(new Set(keys).size).toBe(rows.length);
    expect(keys[0]).toBe('#9');
  });

  it('stay the same for a row when new rows come on top', () => {
    const old = [row(3, 3), row(null, 2)];
    const keys = rowKeys(old);
    expect(rowKeys([row(4, 4), ...old]).slice(1)).toEqual(keys);
  });
});

describe('row texts', () => {
  it('shows only the time for a row of today', () => {
    expect(rowTime('2026-11-14T15:30:05.123+05:00', '2026-11-14')).toBe('15:30:05');
  });

  it('adds the day for another day and the year for another year', () => {
    expect(rowTime('2026-11-13T23:59:59.999+05:00', '2026-11-14')).toBe('13.11 23:59:59');
    expect(rowTime('2025-12-31T08:00:00.000+05:00', '2026-01-01')).toBe('31.12.2025 08:00:00');
  });

  it('shows the time as it was written, whatever the offset', () => {
    expect(rowTime('2026-11-14T03:04:05.000-08:00', '2026-11-14')).toBe('03:04:05');
    expect(rowTime('2026-11-14T03:04:05Z', '2026-11-14')).toBe('03:04:05');
  });

  it('shows what is not a time as it is', () => {
    expect(rowTime('', '2026-11-14')).toBe('');
    expect(rowTime('yesterday', '2026-11-14')).toBe('yesterday');
  });

  it('names the local day', () => {
    expect(dayKey(new Date(2026, 10, 14, 23, 59))).toBe('2026-11-14');
    expect(dayKey(new Date(2026, 0, 5, 0, 0))).toBe('2026-01-05');
  });

  it('names every category and level in both languages', () => {
    for (const lang of ['ru', 'en'] as const) {
      setLang(lang);
      for (const category of CATEGORIES) {
        expect(isMessageKey(`journal.cat.${category}`), category).toBe(true);
        expect(categoryLabel(category), category).not.toBe('');
      }
    }
    expect(CATEGORIES.length).toBe(16);
    for (const level of LEVELS) {
      for (const group of ['level', 'tag', 'write']) expect(isMessageKey(`journal.${group}.${level}`), `${group}.${level}`).toBe(true);
    }
    setLang('ru');
    expect(categoryLabel('tool')).toBe('Инструменты');
    setLang('en');
    expect(categoryLabel('tool')).toBe('Tools');
  });

  it('shows the id of a category it does not know', () => {
    expect(categoryLabel('quantum')).toBe('quantum');
    expect(categoryLabel('')).toBe('');
  });

  it('folds a long message and one of several lines', () => {
    const long = 'x'.repeat(FOLD_AT + 40);
    expect(isLong(long)).toBe(true);
    expect(folded(long)).toBe(`${'x'.repeat(FOLD_AT)}…`);
    const lines = 'request failed: http=500\nbody: {"error":"upstream"}';
    expect(isLong(lines)).toBe(true);
    expect(folded(lines)).toBe('request failed: http=500…');
    expect(isLong('open_app: ok')).toBe(false);
    expect(isLong('x'.repeat(FOLD_AT))).toBe(false);
  });

  it('does not cut a character in two', () => {
    const text = '😀'.repeat(FOLD_AT + 5);
    expect(folded(text)).toBe(`${'😀'.repeat(FOLD_AT)}…`);
  });

  it('copies a row as the fields of the file', () => {
    const r = row(7, 5, { level: 'error', category: 'tool', message: 'run_shell failed\nline two' });
    expect(rowLine(r)).toBe('2026-11-14T15:30:05.000+05:00 ERROR tool run_shell failed\nline two');
  });
});

describe('size and date', () => {
  it('writes a size in bytes, kilobytes and megabytes', () => {
    expect(sizeText(0)).toBe('0 Б');
    expect(sizeText(812)).toBe('812 Б');
    expect(sizeText(1024)).toBe('1 КБ');
    expect(sizeText(700_000)).toBe('684 КБ');
    expect(sizeText(3_565_158)).toBe('3,4 МБ');
    expect(sizeText(5 * 1024 * 1024)).toBe('5 МБ');
    expect(sizeText(-5)).toBe('0 Б');
    expect(sizeText(Number.NaN)).toBe('0 Б');
  });

  it('writes a size in English', () => {
    setLang('en');
    expect(sizeText(700_000)).toBe('684 KB');
    expect(sizeText(3_565_158)).toBe('3.4 MB');
  });

  it('names the day of the first row', () => {
    expect(sinceText('2026-11-01T09:12:44.120+05:00')).toBe('1 ноября');
    // The day as it was written, not the day it is somewhere else.
    expect(sinceText('2026-11-01T00:10:00.000+14:00')).toBe('1 ноября');
    expect(sinceText(null)).toBe('');
    expect(sinceText('soon')).toBe('');
    setLang('en');
    expect(sinceText('2026-11-01T09:12:44.120+05:00')).toBe('1 November');
  });

  it('says how much the journal takes', () => {
    expect(sizeLine(VIEW)).toBe('Журнал занимает 3,4 МБ, файлов: 3, записи с 1 ноября.');
    expect(sizeLine({ size_bytes: 0, files: 1, oldest: null })).toBe('Журнал занимает 0 Б, файлов: 1.');
    setLang('en');
    expect(sizeLine(VIEW)).toBe('The log takes 3.4 MB, files: 3, rows since 1 November.');
    // One file is the state of every install until the first rotation: no plural to get wrong.
    expect(sizeLine({ size_bytes: 0, files: 1, oldest: null })).toBe('The log takes 0 B, files: 1.');
  });
});

describe('settings', () => {
  it('sends the four settings and nothing else', () => {
    expect(settingsOf(VIEW)).toEqual({ level: 'info', content: false, keep_days: 14, keep_files: 5 });
  });

  it('reads the days typed into the field', () => {
    expect(cleanDays('30', 14)).toBe(30);
    expect(cleanDays(' 7 ', 14)).toBe(7);
    expect(cleanDays('2.6', 14)).toBe(3);
    expect(cleanDays('0', 14)).toBe(1);
    expect(cleanDays('-3', 14)).toBe(1);
    expect(cleanDays('9999', 14)).toBe(365);
    expect(cleanDays('', 14)).toBe(14);
    expect(cleanDays('week', 14)).toBe(14);
    expect(cleanDays(null, 14)).toBe(14);
    expect(cleanDays(21, 14)).toBe(21);
  });
});
