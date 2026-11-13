import { describe, expect, it } from 'vitest';
import {
  BREAK_RANGE,
  REMIND_RANGE,
  calendarArgs,
  linkAfterSave,
  rhythmArgs,
  uniqueFolders,
  wholeMinutes,
  withFolder,
  withoutFolder,
} from './rhythm';

const saved = { breaks: true, break_every_min: 50, lively: true };

describe('wholeMinutes', () => {
  it('keeps the saved value for an empty field', () => {
    expect(wholeMinutes(null, BREAK_RANGE, 50)).toBe(50);
    expect(wholeMinutes(undefined, REMIND_RANGE, 10)).toBe(10);
    expect(wholeMinutes(Number.NaN, REMIND_RANGE, 10)).toBe(10);
  });
  it('rounds and clamps what the core would refuse or change', () => {
    expect(wholeMinutes(5, BREAK_RANGE, 50)).toBe(15);
    expect(wholeMinutes(500, BREAK_RANGE, 50)).toBe(240);
    expect(wholeMinutes(37.6, BREAK_RANGE, 50)).toBe(38);
    expect(wholeMinutes(-3, REMIND_RANGE, 10)).toBe(0);
    expect(wholeMinutes(0, REMIND_RANGE, 10)).toBe(0);
    expect(wholeMinutes(2.5, REMIND_RANGE, 10)).toBe(3);
  });
});

describe('rhythmArgs', () => {
  it('never sends an empty minutes field', () => {
    // The field was wiped, then «Живость» turned off: both must still save.
    expect(rhythmArgs(saved, null, { lively: false })).toEqual({ breaks: true, breakEveryMin: 50, lively: false });
  });
  it('sends the typed minutes, whole and in range', () => {
    expect(rhythmArgs(saved, 30, {})).toEqual({ breaks: true, breakEveryMin: 30, lively: true });
    expect(rhythmArgs(saved, 5)).toEqual({ breaks: true, breakEveryMin: 15, lively: true });
  });
});

describe('calendarArgs', () => {
  it('never sends an empty reminder field', () => {
    expect(calendarArgs(true, null, 10, '')).toEqual({ enabled: true, remindMin: 10, url: null });
  });
  it('sends the link trimmed', () => {
    expect(calendarArgs(false, 200, 10, '  webcal://x/cal.ics ')).toEqual({
      enabled: false,
      remindMin: 120,
      url: 'webcal://x/cal.ics',
    });
  });
});

describe('linkAfterSave', () => {
  it('keeps a refused link to be corrected', () => {
    expect(linkAfterSave(false, 'http://x/cal.ics', 'http://x/cal.ics')).toBe('http://x/cal.ics');
  });
  it('empties the field once the link is kept', () => {
    expect(linkAfterSave(true, ' https://x/cal.ics', ' https://x/cal.ics')).toBe('');
  });
  it('keeps a link typed while saving', () => {
    expect(linkAfterSave(true, 'https://x/a.ics', 'https://x/b.ics')).toBe('https://x/b.ics');
  });
});

describe('folders', () => {
  it('drops repeats, keeping the first spelling', () => {
    expect(uniqueFolders(['D:\\Договоры', 'D:\\Договоры', 'd:/договоры/', 'E:\\Работа'])).toEqual([
      'D:\\Договоры',
      'E:\\Работа',
    ]);
  });
  it('does not add a folder that is already there', () => {
    expect(withFolder(['D:\\Договоры'], 'D:\\Договоры')).toBeNull();
    expect(withFolder(['D:\\Договоры'], ' d:\\ДОГОВОРЫ\\ ')).toBeNull();
    expect(withFolder(['D:\\Договоры'], '  ')).toBeNull();
    expect(withFolder(['D:\\Договоры'], ' E:\\Работа ')).toEqual(['D:\\Договоры', 'E:\\Работа']);
  });
  it('mends a list that already has a repeat', () => {
    expect(withFolder(['D:\\Договоры', 'D:\\Договоры'], 'E:\\Работа')).toEqual(['D:\\Договоры', 'E:\\Работа']);
  });
  it('removes every spelling of a folder', () => {
    expect(withoutFolder(['D:\\Договоры', 'E:\\Работа', 'd:/договоры'], 'D:\\Договоры')).toEqual(['E:\\Работа']);
  });
});
