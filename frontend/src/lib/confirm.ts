// Open confirmation request folded from bus events. Pure functions.

import type { MessageKey } from './i18n';

export type RiskLevel = 'low' | 'medium' | 'high';

export interface Assessment {
  level: RiskLevel;
  summary: string;
  target: string;
  reason: string;
  scope: string;
}

export interface Pending {
  id: number;
  tool: string;
  assessment: Assessment;
  allowAlways: boolean;
  /** ms timestamp when the request expires. */
  deadline: number;
}

/**
 * A question whose target is shown whole, without a scroll box. A scenario by
 * event runs later by itself, so its limits and its whole instruction must be
 * in sight when it is confirmed (decision 0031).
 */
export function showsWhole(tool: string): boolean {
  return tool === 'trigger_create';
}

/** Heading of the confirmation card for a risk level. */
export function confirmHead(level: RiskLevel): MessageKey {
  return `confirm.head_${level}`;
}

/** Why "allow always" is offered or not, as a tooltip. */
export function alwaysTitle(p: Pending): MessageKey {
  if (p.allowAlways) return 'confirm.always_title';
  return p.assessment.level === 'high' ? 'confirm.always_high' : 'confirm.always_wide';
}

/**
 * Whether the card holds a text the English interface cannot show in English yet:
 * the summary and the reason come from Rust, and there they are still Russian.
 */
export function untranslated(p: Pending, lang: string): boolean {
  return lang === 'en' && /[\u0400-\u04ff]/.test(`${p.assessment.summary} ${p.assessment.reason}`);
}

function isAssessment(v: unknown): v is Assessment {
  if (typeof v !== 'object' || v === null) return false;
  const a = v as Record<string, unknown>;
  return (
    (a.level === 'low' || a.level === 'medium' || a.level === 'high') &&
    typeof a.summary === 'string' &&
    typeof a.target === 'string' &&
    typeof a.reason === 'string'
  );
}

export function reducePending(p: Pending | null, event: unknown, now: number): Pending | null {
  if (typeof event !== 'object' || event === null) return p;
  const e = event as Record<string, unknown>;
  if (e.type === 'confirm_requested') {
    if (typeof e.id !== 'number' || typeof e.tool !== 'string' || !isAssessment(e.assessment)) return p;
    const timeout = typeof e.timeout_s === 'number' ? e.timeout_s : 60;
    return {
      id: e.id,
      tool: e.tool,
      assessment: e.assessment,
      allowAlways: e.allow_always === true && e.assessment.level !== 'high',
      deadline: now + timeout * 1000,
    };
  }
  if (e.type === 'confirm_resolved') {
    return p && e.id === p.id ? null : p;
  }
  return p;
}

export function secondsLeft(p: Pending, now: number): number {
  return Math.max(0, Math.ceil((p.deadline - now) / 1000));
}
