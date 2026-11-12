import { describe, expect, it } from 'vitest';
import { reduceChat } from './chat';
import { progress, reduceTask, type TaskView } from './task';

const task = (finished = false): unknown => ({
  type: 'task_updated',
  task: {
    title: 'Разобрать загрузки',
    steps: [
      { text: 'Список', status: 'done', note: '12 файлов' },
      { text: 'Перенос', status: 'active' },
      { text: 'Проверка', status: 'pending' },
    ],
    finished,
    ok: false,
  },
});

describe('reduceTask', () => {
  it('takes the whole task from task_updated', () => {
    const t = reduceTask(null, task()) as TaskView;
    expect(t.title).toBe('Разобрать загрузки');
    expect(t.steps[0].note).toBe('12 файлов');
    expect(t.steps[1].note).toBeUndefined();
    expect(progress(t)).toEqual({ done: 1, total: 3 });
  });

  it('skipped steps are not counted as done', () => {
    const t = reduceTask(null, {
      type: 'task_updated',
      task: { title: 't', steps: [{ text: 'a', status: 'done' }, { text: 'b', status: 'skipped' }], finished: true, ok: false },
    }) as TaskView;
    expect(progress(t)).toEqual({ done: 1, total: 2 });
  });

  it('ignores broken payloads', () => {
    const t = reduceTask(null, task());
    expect(reduceTask(t, { type: 'task_updated', task: { title: 'x', steps: [{ text: 'a', status: 'weird' }] } })).toBe(t);
    expect(reduceTask(t, null)).toBe(t);
  });

  it('a new message hides a finished task only', () => {
    const running = reduceTask(null, task());
    expect(reduceTask(running, { type: 'user_text', text: 'ещё' })).toBe(running);
    const done = reduceTask(null, task(true));
    expect(reduceTask(done, { type: 'user_text', text: 'ещё' })).toBeNull();
  });
});

describe('chat hides task tools', () => {
  it('task_plan and task_step make no chat rows', () => {
    let items = reduceChat([], { type: 'tool_started', id: 'p', name: 'task_plan', summary: 'Составить план' });
    items = reduceChat(items, { type: 'tool_finished', id: 'p', name: 'task_plan', ok: true, output: '', elapsed_ms: 1 });
    expect(items).toEqual([]);
    items = reduceChat(items, { type: 'tool_started', id: 'e', name: 'list_folder', summary: 'Посмотреть папку' });
    expect(items).toHaveLength(1);
  });
});
