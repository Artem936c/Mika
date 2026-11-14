// Backup and transfer to another PC: the shapes the Rust commands return and
// the few choices the page makes from them. No command is called from here.

import type { TriggerAction } from './events';
import type { MessageKey } from './i18n';

export type CategoryId =
  | 'settings'
  | 'history'
  | 'memory'
  | 'notes'
  | 'todos'
  | 'reminders'
  | 'tasks'
  | 'commands'
  | 'scenarios'
  | 'rules_deny'
  | 'rules_allow'
  | 'secrets';

/** The order of the preview, which is the order a selection is sent in. */
export const CATEGORY_ORDER: CategoryId[] = [
  'settings',
  'history',
  'memory',
  'notes',
  'todos',
  'reminders',
  'tasks',
  'commands',
  'scenarios',
  'rules_deny',
  'rules_allow',
  'secrets',
];

export interface BackupFile {
  path: string;
  name: string;
  bytes: number;
  created_at: string;
  device: string;
  app_version: string;
  encrypted: boolean;
  keys: number;
  newer_app: boolean;
}

export interface CategoryRow {
  id: CategoryId;
  in_file: number;
  here: number;
  available: boolean;
  selected: boolean;
}

export interface RuleRow {
  tool: string;
  pattern: string;
}

/** A scheduled task of the copy: a text the assistant gets as a message when the time comes. */
export interface TaskRow {
  text: string;
  /** Unix seconds. */
  due_at: number;
  /** `none`, `daily`, `weekly` or `every:<minutes>`. */
  repeat: string;
}

/**
 * A scenario by event of the copy: what happens, and what the assistant then
 * does by herself. It comes back switched off and without its counters.
 */
export interface ScenarioRow {
  name: string;
  /** One of the ten event ids, `audio_connected` … `user_back`. */
  event: string;
  /** Part of a device name or a process name; empty when the event takes none. */
  filter: string;
  idle_min: number;
  action: TriggerAction;
  cooldown_s: number;
  max_per_day: number;
  /** For a scenario of a saved command: the copy holds a command of that name. */
  command_in_file: boolean;
  /** For a scenario of a saved command: this PC holds a command of that name. */
  command_here: boolean;
}

export interface KeyRow {
  kind: 'llm_openai' | 'llm_anthropic' | 'llm_ollama' | 'speech' | 'image' | 'embedding' | 'telegram' | 'calendar' | 'mcp';
  name: string | null;
  replaces?: boolean;
}

export interface AddressRow {
  purpose: 'llm' | 'speech' | 'image' | 'embedding';
  incoming: string;
  current: string;
  changed: boolean;
  key_here: boolean;
  key_in_file: boolean;
}

/** Something of the copy that a token of this PC would follow: a server address, a Telegram owner. */
export interface TokenTarget {
  changed: boolean;
  token_here: boolean;
  token_in_file: boolean;
}

export interface McpRow extends TokenTarget {
  name: string;
  target: string;
  was_enabled: boolean;
  /** The target of the server of that name on this PC; empty when there is none. */
  current: string;
}

export interface TelegramRow extends TokenTarget {
  was_enabled: boolean;
  user_id: number;
  /** The owner on this PC; 0 when none is set. */
  current: number;
}

export type OffItem =
  | {
      id:
        | 'microphone'
        | 'screen'
        | 'notifications'
        | 'app_time'
        | 'clipboard'
        | 'dictation'
        | 'screen_ask'
        | 'telegram'
        | 'telegram_voice'
        | 'web'
        | 'calendar'
        | 'briefing'
        | 'evening'
        | 'events'
        | 'journal_content';
    }
  | { id: 'mcp' | 'docs' | 'scenarios'; count: number };

export interface BackupPreview {
  file: BackupFile;
  categories: CategoryRow[];
  allow_rules: RuleRow[];
  allow_rules_total: number;
  allow_rules_dropped: number;
  tasks: TaskRow[];
  tasks_total: number;
  /** The scenarios by event a restore would write, every one switched off. */
  scenarios: ScenarioRow[];
  scenarios_total: number;
  secrets: KeyRow[];
  addresses: AddressRow[];
  mcp: McpRow[];
  telegram: TelegramRow | null;
  switched_off: OffItem[];
  docs_folders: string[];
  schema_newer: boolean;
  skipped_unknown: number;
  dropped_rows: number;
  safety_copy: string;
}

export interface ExportResult {
  path: string;
  bytes: number;
  encrypted: boolean;
  keys: number;
}

export interface RestoreResult {
  restored: { id: CategoryId; count: number }[];
  keys_written: number;
  keys_failed: KeyRow[];
  switched_off: OffItem[];
  docs_folders: string[];
  /** Scenarios left out: the saved command they name is neither restored nor on this PC. */
  scenarios_without_command: number;
  restart_needed: boolean;
  config_error: string | null;
  safety_copy: string;
}

/** What is ticked when the preview opens: what Rust marked as selected. */
export function initialSelection(p: BackupPreview): CategoryId[] {
  return p.categories.filter((c) => c.selected).map((c) => c.id);
}

/** The selection with `id` added or taken out, in the order of the preview. */
export function toggled(ids: CategoryId[], id: CategoryId): CategoryId[] {
  const next = ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id];
  return CATEGORY_ORDER.filter((c) => next.includes(c));
}

/**
 * Whether restoring would send a key of this PC to an address from the copy:
 * the settings are ticked, the address differs, a key for it is stored here,
 * and the copy does not replace that key by its own.
 */
export function addressWarns(a: AddressRow, selected: CategoryId[]): boolean {
  return selected.includes('settings') && a.changed && a.key_here && !(a.key_in_file && selected.includes('secrets'));
}

/**
 * Whether a token of this PC would follow a target from the copy: the
 * settings are ticked, the target differs, a token for it is stored here,
 * and the copy does not replace that token by its own.
 */
export function tokenWarns(row: TokenTarget, selected: CategoryId[]): boolean {
  return selected.includes('settings') && row.changed && row.token_here && !(row.token_in_file && selected.includes('secrets'));
}

/**
 * Whether a scenario of the copy would come back with this selection. One
 * that names a saved command needs that command after the restore: ticked
 * commands replace the ones of this PC (when the copy has any), unticked
 * ones leave them. Rust decides the same at the restore, from the database.
 */
export function scenarioComes(s: ScenarioRow, selected: CategoryId[], commandsInFile: number): boolean {
  if (s.action.kind !== 'command') return true;
  const replaced = selected.includes('commands') && commandsInFile > 0;
  return replaced ? s.command_in_file : s.command_here;
}

/** How often a scheduled task repeats, as a text key; null for one that runs once. */
export function repeatLabel(repeat: string): { key: MessageKey; params?: Record<string, number> } | null {
  if (repeat === 'daily') return { key: 'backup.repeat.daily' };
  if (repeat === 'weekly') return { key: 'backup.repeat.weekly' };
  const every = /^every:(\d+)$/.exec(repeat);
  return every ? { key: 'backup.repeat.every', params: { min: Number(every[1]) } } : null;
}

/** Name of a key for the user. The value never reaches the page. */
export function keyLabel(k: KeyRow): { key: MessageKey; params?: Record<string, string> } {
  const key: MessageKey = `backup.key.${k.kind}`;
  return k.kind === 'mcp' ? { key, params: { name: k.name ?? '' } } : { key };
}

/** A line of the "will be switched off" list. */
export function offLabel(o: OffItem): { key: MessageKey; params?: Record<string, number> } {
  const key: MessageKey = `backup.off.${o.id}`;
  return 'count' in o ? { key, params: { count: o.count } } : { key };
}
