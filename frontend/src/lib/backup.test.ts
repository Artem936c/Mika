import { describe, expect, it } from 'vitest';
import {
  CATEGORY_ORDER,
  addressWarns,
  initialSelection,
  keyLabel,
  offLabel,
  repeatLabel,
  scenarioComes,
  toggled,
  tokenWarns,
  type AddressRow,
  type BackupPreview,
  type CategoryId,
  type KeyRow,
  type ScenarioRow,
} from './backup';
import { isMessageKey, t } from './i18n';

function preview(selected: CategoryId[], available: CategoryId[] = CATEGORY_ORDER): BackupPreview {
  return {
    file: { path: 'D:\\copy.mikabackup', name: 'copy.mikabackup', bytes: 1, created_at: '', device: '', app_version: '', encrypted: true, keys: 0, newer_app: false },
    categories: CATEGORY_ORDER.map((id) => ({ id, in_file: 1, here: 0, available: available.includes(id), selected: selected.includes(id) })),
    allow_rules: [],
    allow_rules_total: 0,
    allow_rules_dropped: 0,
    tasks: [],
    tasks_total: 0,
    scenarios: [],
    scenarios_total: 0,
    secrets: [],
    addresses: [],
    mcp: [],
    telegram: null,
    switched_off: [],
    docs_folders: [],
    schema_newer: false,
    skipped_unknown: 0,
    dropped_rows: 0,
    safety_copy: '',
  };
}

describe('initialSelection', () => {
  it('takes what Rust marked, in the order of the preview', () => {
    expect(initialSelection(preview(['notes', 'settings', 'secrets']))).toEqual(['settings', 'notes', 'secrets']);
    expect(initialSelection(preview([]))).toEqual([]);
  });

  it('does not tick the always-allow rules and the scheduled tasks by itself', () => {
    const all = CATEGORY_ORDER.filter((id) => id !== 'rules_allow' && id !== 'tasks');
    expect(initialSelection(preview(all))).not.toContain('rules_allow');
    expect(initialSelection(preview(all))).not.toContain('tasks');
    expect(initialSelection(preview(all))).toHaveLength(10);
    expect(CATEGORY_ORDER).toHaveLength(12);
    // Scenarios by event stand right after the commands they may name.
    expect(CATEGORY_ORDER.indexOf('scenarios')).toBe(CATEGORY_ORDER.indexOf('commands') + 1);
  });
});

describe('scenarioComes', () => {
  const scenario = (kind: 'command' | 'instruction', inFile: boolean, here: boolean): ScenarioRow => ({
    name: 'Работа',
    event: 'app_started',
    filter: 'code.exe',
    idle_min: 0,
    action: { kind, text: kind === 'command' ? 'Рабочий режим' : 'Открой Блокнот' },
    cooldown_s: 60,
    max_per_day: 20,
    command_in_file: inFile,
    command_here: here,
  });

  it('an instruction needs nothing else', () => {
    expect(scenarioComes(scenario('instruction', false, false), ['scenarios'], 0)).toBe(true);
    expect(scenarioComes(scenario('instruction', false, false), ['scenarios', 'commands'], 3)).toBe(true);
  });

  it('a saved command must be there after the restore', () => {
    const inFile = scenario('command', true, false);
    const here = scenario('command', false, true);
    const nowhere = scenario('command', false, false);
    // The commands are ticked: those of the copy replace the ones here.
    expect(scenarioComes(inFile, ['commands', 'scenarios'], 2)).toBe(true);
    expect(scenarioComes(here, ['commands', 'scenarios'], 2)).toBe(false);
    // Not ticked: the commands of this PC stay.
    expect(scenarioComes(inFile, ['scenarios'], 2)).toBe(false);
    expect(scenarioComes(here, ['scenarios'], 2)).toBe(true);
    expect(scenarioComes(nowhere, ['scenarios'], 2)).toBe(false);
    expect(scenarioComes(nowhere, ['commands', 'scenarios'], 2)).toBe(false);
    // A copy without commands deletes none, ticked or not.
    expect(scenarioComes(here, ['commands', 'scenarios'], 0)).toBe(true);
  });

  it('has its texts', () => {
    for (const key of ['backup.scenario', 'backup.scenarios_sub', 'backup.scenario_no_command', 'backup.scenarios_more', 'backup.done_scenarios_skipped']) {
      expect(isMessageKey(key), key).toBe(true);
    }
    expect(t('backup.scenario', { name: 'Работа', when: 'запущена программа code.exe', action: 'команда «Рабочий режим»', pause: '5 мин', max: 5 })).toBe(
      '«Работа»: когда запущена программа code.exe — команда «Рабочий режим»; пауза 5 мин, до 5 в день',
    );
  });
});

describe('toggled', () => {
  it('adds and removes', () => {
    expect(toggled(['settings'], 'notes')).toEqual(['settings', 'notes']);
    expect(toggled(['settings', 'notes'], 'settings')).toEqual(['notes']);
    expect(toggled([], 'secrets')).toEqual(['secrets']);
    expect(toggled(['secrets'], 'secrets')).toEqual([]);
  });

  it('keeps the order of the preview whatever was clicked first', () => {
    let ids: CategoryId[] = [];
    for (const id of ['secrets', 'rules_allow', 'tasks', 'history', 'settings'] as CategoryId[]) ids = toggled(ids, id);
    expect(ids).toEqual(['settings', 'history', 'tasks', 'rules_allow', 'secrets']);
  });

  it('does not change the list it was given', () => {
    const ids: CategoryId[] = ['notes'];
    toggled(ids, 'todos');
    expect(ids).toEqual(['notes']);
  });
});

describe('addressWarns', () => {
  const row = (over: Partial<AddressRow>): AddressRow => ({
    purpose: 'llm',
    incoming: 'https://routerai.ru/api/v1',
    current: 'https://api.openai.com/v1',
    changed: true,
    key_here: true,
    key_in_file: false,
    ...over,
  });

  it('warns when a key of this PC would go to an address from the copy', () => {
    expect(addressWarns(row({}), ['settings'])).toBe(true);
    expect(addressWarns(row({}), ['settings', 'secrets'])).toBe(true);
  });

  it('needs the settings to be ticked', () => {
    expect(addressWarns(row({}), [])).toBe(false);
    expect(addressWarns(row({}), ['history', 'secrets'])).toBe(false);
  });

  it('needs a changed address', () => {
    expect(addressWarns(row({ changed: false }), ['settings'])).toBe(false);
  });

  it('needs a key on this PC', () => {
    expect(addressWarns(row({ key_here: false }), ['settings'])).toBe(false);
  });

  it('is quiet only when the copy brings the key and the keys are ticked', () => {
    expect(addressWarns(row({ key_in_file: true }), ['settings', 'secrets'])).toBe(false);
    // The key stays the one of this PC: it would still go to the new address.
    expect(addressWarns(row({ key_in_file: true }), ['settings'])).toBe(true);
  });
});

describe('tokenWarns', () => {
  const row = (over: Partial<{ changed: boolean; token_here: boolean; token_in_file: boolean }>) => ({
    changed: true,
    token_here: true,
    token_in_file: false,
    ...over,
  });

  it('warns when a token of this PC would follow a target from the copy', () => {
    expect(tokenWarns(row({}), ['settings'])).toBe(true);
    expect(tokenWarns(row({}), ['settings', 'secrets'])).toBe(true);
  });

  it('needs the settings ticked, a changed target and a token here', () => {
    expect(tokenWarns(row({}), ['secrets'])).toBe(false);
    expect(tokenWarns(row({ changed: false }), ['settings'])).toBe(false);
    expect(tokenWarns(row({ token_here: false }), ['settings'])).toBe(false);
  });

  it('is quiet only when the copy brings the token and the keys are ticked', () => {
    expect(tokenWarns(row({ token_in_file: true }), ['settings', 'secrets'])).toBe(false);
    expect(tokenWarns(row({ token_in_file: true }), ['settings'])).toBe(true);
  });
});

describe('repeatLabel', () => {
  it('names how often a task repeats', () => {
    expect(repeatLabel('daily')).toEqual({ key: 'backup.repeat.daily' });
    expect(repeatLabel('weekly')).toEqual({ key: 'backup.repeat.weekly' });
    const every = repeatLabel('every:30');
    expect(every).toEqual({ key: 'backup.repeat.every', params: { min: 30 } });
    expect(t(every!.key, every!.params)).toContain('30');
  });

  it('says nothing for a task that runs once or a value it does not know', () => {
    expect(repeatLabel('none')).toBeNull();
    expect(repeatLabel('')).toBeNull();
    expect(repeatLabel('every:x')).toBeNull();
    expect(repeatLabel('hourly')).toBeNull();
  });
});

describe('keyLabel', () => {
  it('names every kind of key', () => {
    const kinds: KeyRow['kind'][] = ['llm_openai', 'llm_anthropic', 'llm_ollama', 'speech', 'image', 'embedding', 'telegram', 'calendar', 'mcp'];
    for (const kind of kinds) {
      const label = keyLabel({ kind, name: kind === 'mcp' ? 'files' : null });
      expect(isMessageKey(label.key), kind).toBe(true);
    }
    expect(keyLabel({ kind: 'telegram', name: null })).toEqual({ key: 'backup.key.telegram' });
  });

  it('carries the server name of an MCP token', () => {
    const label = keyLabel({ kind: 'mcp', name: 'files' });
    expect(label).toEqual({ key: 'backup.key.mcp', params: { name: 'files' } });
    expect(t(label.key, label.params)).toContain('files');
    expect(keyLabel({ kind: 'mcp', name: null }).params).toEqual({ name: '' });
  });
});

describe('offLabel', () => {
  it('names a switch', () => {
    expect(offLabel({ id: 'microphone' })).toEqual({ key: 'backup.off.microphone' });
    expect(offLabel({ id: 'app_time' })).toEqual({ key: 'backup.off.app_time' });
  });

  it('carries the count of servers and folders', () => {
    const mcp = offLabel({ id: 'mcp', count: 2 });
    expect(mcp).toEqual({ key: 'backup.off.mcp', params: { count: 2 } });
    expect(t(mcp.key, mcp.params)).toContain('2');
    expect(t(offLabel({ id: 'docs', count: 1 }).key, { count: 1 })).not.toContain('{count}');
  });

  it('has a text for every item Rust can send', () => {
    for (const id of [
      'microphone',
      'screen',
      'notifications',
      'app_time',
      'clipboard',
      'dictation',
      'screen_ask',
      'telegram',
      'telegram_voice',
      'web',
      'calendar',
      'briefing',
      'evening',
      'events',
      'journal_content',
      'mcp',
      'docs',
      'scenarios',
    ]) {
      expect(isMessageKey(`backup.off.${id}`), id).toBe(true);
    }
    expect(offLabel({ id: 'scenarios', count: 3 })).toEqual({ key: 'backup.off.scenarios', params: { count: 3 } });
    expect(offLabel({ id: 'dictation' })).toEqual({ key: 'backup.off.dictation' });
  });
});

describe('texts', () => {
  it('every category has a name', () => {
    for (const id of CATEGORY_ORDER) expect(isMessageKey(`backup.cat.${id}`), id).toBe(true);
  });

  it('every address purpose has a line', () => {
    for (const purpose of ['llm', 'speech', 'image', 'embedding']) {
      expect(isMessageKey(`backup.address.${purpose}`), purpose).toBe(true);
    }
  });
});
