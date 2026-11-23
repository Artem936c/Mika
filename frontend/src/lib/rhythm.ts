// Fields of the "Rhythm of the day" settings. Pure functions, no Tauri calls.

export interface Rhythm {
  breaks: boolean;
  break_every_min: number;
  lively: boolean;
}

/** Minutes of work before a break reminder, as the core keeps them. */
export const BREAK_RANGE: [number, number] = [15, 240];
/** Minutes before a meeting; 0 turns the reminder off. */
export const REMIND_RANGE: [number, number] = [0, 120];

/**
 * Whole minutes within the range. An empty number field (`bind:value` gives
 * `null`) or a broken one keeps the saved value: the core takes only `u32`.
 */
export function wholeMinutes(value: unknown, [min, max]: [number, number], saved: number): number {
  const n = typeof value === 'number' && Number.isFinite(value) ? value : saved;
  return Math.min(max, Math.max(min, Math.round(n)));
}

/** Arguments of `set_rhythm`, in camelCase as Tauri takes them. */
export function rhythmArgs(saved: Rhythm, every: unknown, change: Partial<Rhythm> = {}) {
  const r = { ...saved, break_every_min: wholeMinutes(every, BREAK_RANGE, saved.break_every_min), ...change };
  return { breaks: r.breaks, breakEveryMin: r.break_every_min, lively: r.lively };
}

/** The companion in games (decision 0054), as `get_rhythm_settings` gives it. */
export interface Companion {
  enabled: boolean;
  every_s: number;
  max_per_hour: number;
}

/** Seconds between two looks of the companion, as the core keeps them. */
export const COMPANION_RANGE: [number, number] = [20, 300];

/** Arguments of `set_companion`, in camelCase as Tauri takes them. An empty field keeps the saved interval. */
export function companionArgs(saved: Companion, every: unknown, enabled = saved.enabled) {
  return { enabled, everyS: wholeMinutes(every, COMPANION_RANGE, saved.every_s) };
}

/**
 * What the switch of the companion does when flipped: turning on asks first,
 * and without the Screen switch it only explains; turning off goes at once.
 */
export function companionFlip(on: boolean, saved: boolean, screen: boolean): 'ask' | 'needs_screen' | 'off' | 'none' {
  if (on && !saved) return screen ? 'ask' : 'needs_screen';
  if (!on && saved) return 'off';
  return 'none';
}

/** Arguments of `save_calendar`. An empty link field leaves the saved link as it is. */
export function calendarArgs(enabled: boolean, remind: unknown, savedRemind: number, url: string) {
  const link = url.trim();
  return { enabled, remindMin: wholeMinutes(remind, REMIND_RANGE, savedRemind), url: link ? link : null };
}

/**
 * What stays in the secret link field after saving. A refused link stays to be
 * corrected: the field hides it, and getting it again means a trip to the
 * calendar settings. A link typed while saving stays too.
 */
export function linkAfterSave(saved: boolean, sent: string, field: string): string {
  return saved && field.trim() === sent.trim() ? '' : field;
}

/** One folder written another way: letter case, slashes, a separator at the end. */
function folderKey(path: string): string {
  return path.trim().replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

/** Folders without repeats; the first spelling stays. */
export function uniqueFolders(folders: string[]): string[] {
  const seen = new Set<string>();
  return folders.filter((f) => {
    const key = folderKey(f);
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** The list with one more folder, or `null` when the field is empty or the folder is already there. */
export function withFolder(folders: string[], folder: string): string[] | null {
  const f = folder.trim();
  const key = folderKey(f);
  if (!key || folders.some((x) => folderKey(x) === key)) return null;
  return [...uniqueFolders(folders), f];
}

/** The list without the folder, however many times it was written. */
export function withoutFolder(folders: string[], folder: string): string[] {
  const key = folderKey(folder);
  return uniqueFolders(folders.filter((x) => folderKey(x) !== key));
}
