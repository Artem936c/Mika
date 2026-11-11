// Open confirmation request folded from bus events. Pure functions.

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

export const RISK_LABELS: Record<RiskLevel, string> = {
  low: 'низкий',
  medium: 'средний',
  high: 'высокий',
};

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
