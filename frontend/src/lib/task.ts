// The running multi-step task, as the core sends it in `task_updated`. Pure functions.

export type StepStatus = 'pending' | 'active' | 'done' | 'failed' | 'skipped';

export interface TaskStep {
  text: string;
  status: StepStatus;
  note?: string;
}

export interface TaskView {
  title: string;
  steps: TaskStep[];
  finished: boolean;
  ok: boolean;
}

/** Tools that only move the task board: the panel shows them, the chat does not. */
export const TASK_TOOLS = ['task_plan', 'task_step'];

const STATUSES: StepStatus[] = ['pending', 'active', 'done', 'failed', 'skipped'];

function parseTask(v: unknown): TaskView | null {
  if (typeof v !== 'object' || v === null) return null;
  const t = v as Record<string, unknown>;
  if (typeof t.title !== 'string' || !Array.isArray(t.steps)) return null;
  const steps: TaskStep[] = [];
  for (const s of t.steps) {
    if (typeof s !== 'object' || s === null) return null;
    const step = s as Record<string, unknown>;
    if (typeof step.text !== 'string' || !STATUSES.includes(step.status as StepStatus)) return null;
    steps.push({
      text: step.text,
      status: step.status as StepStatus,
      note: typeof step.note === 'string' && step.note ? step.note : undefined,
    });
  }
  return { title: t.title, steps, finished: t.finished === true, ok: t.ok === true };
}

/**
 * The task after a bus event. A new user message hides a finished task;
 * an unfinished one stays until the core closes it.
 */
export function reduceTask(current: TaskView | null, event: unknown): TaskView | null {
  if (typeof event !== 'object' || event === null) return current;
  const e = event as { type?: unknown; task?: unknown };
  if (e.type === 'task_updated') return parseTask(e.task) ?? current;
  if (e.type === 'user_text' && current?.finished) return null;
  return current;
}

/** Steps done or skipped, out of all. */
export function progress(task: TaskView): { done: number; total: number } {
  const done = task.steps.filter((s) => s.status === 'done' || s.status === 'skipped').length;
  return { done, total: task.steps.length };
}
