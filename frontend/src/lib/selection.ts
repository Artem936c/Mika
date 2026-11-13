/** Text the user selected in another program and sent by the shortcut. */
export interface Selection {
  text: string;
  /** Program it came from: `winword.exe`. */
  app: string;
}

/** Buttons of the selection card, in order. */
export const SELECTION_ACTIONS = ['translate', 'fix', 'rephrase', 'shorter', 'explain'] as const;

/** Actions whose answer can replace the selection. */
export const PASTABLE: ReadonlySet<string> = new Set(['translate', 'fix', 'rephrase', 'shorter']);

/** The selection from a `mika://ui` payload, or null. */
export function selectionFrom(payload: unknown): Selection | null {
  const p = payload as { action?: unknown; text?: unknown; app?: unknown } | null;
  if (p?.action !== 'selection' || typeof p.text !== 'string' || !p.text.trim()) return null;
  return { text: p.text, app: typeof p.app === 'string' ? p.app : '' };
}
