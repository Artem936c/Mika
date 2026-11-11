import { describe, expect, it } from 'vitest';
import { RISK_LABELS, reducePending, secondsLeft, type Pending } from './confirm';

const assessment = { level: 'medium', summary: 'Открыть', target: 'notepad', reason: 'запуск', scope: 'notepad' };
const req = { type: 'confirm_requested', id: 7, tool: 'open_app', assessment, allow_always: true, timeout_s: 60 };

describe('reducePending', () => {
  it('confirm_requested opens', () => {
    expect(reducePending(null, req, 1000)).toEqual({
      id: 7,
      tool: 'open_app',
      assessment,
      allowAlways: true,
      deadline: 61_000,
    });
  });
  it('high risk never allows always even if the event says so', () => {
    const p = reducePending(null, { ...req, assessment: { ...assessment, level: 'high' } }, 0);
    expect(p?.allowAlways).toBe(false);
  });
  it('confirm_resolved with same id closes', () => {
    const p = reducePending(null, req, 0);
    expect(reducePending(p, { type: 'confirm_resolved', id: 7, answer: 'once', by: 'button' }, 0)).toBeNull();
  });
  it('other id is ignored', () => {
    const p = reducePending(null, req, 0);
    expect(reducePending(p, { type: 'confirm_resolved', id: 8 }, 0)).toBe(p);
  });
  it('malformed ignored', () => {
    expect(reducePending(null, { type: 'confirm_requested', id: 'x' }, 0)).toBeNull();
    expect(reducePending(null, { type: 'confirm_requested', id: 1, tool: 't', assessment: { level: 'huge' } }, 0)).toBeNull();
    expect(reducePending(null, null, 0)).toBeNull();
  });
});

describe('secondsLeft', () => {
  it('counts down and stops at 0', () => {
    const p = reducePending(null, req, 0) as Pending;
    expect(secondsLeft(p, 0)).toBe(60);
    expect(secondsLeft(p, 59_001)).toBe(1);
    expect(secondsLeft(p, 61_000)).toBe(0);
  });
});

describe('RISK_LABELS', () => {
  it('is Russian', () => {
    expect(RISK_LABELS).toEqual({ low: 'низкий', medium: 'средний', high: 'высокий' });
  });
});
