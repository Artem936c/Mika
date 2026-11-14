// Global hotkeys, dictation and the question about the screen: what the page
// reads from the app. The rules themselves live in Rust (`mika_core::hotkeys`).

/** One row of the hotkeys page. */
export interface HotkeyRow {
  /** Action id: `chat`, `listen`, `selection`, `dictation`, `screen_ask`. */
  id: string;
  /** The combination as text: `Ctrl+Alt+D`. */
  combo: string;
  /** The user chose it; otherwise it is a default. */
  custom: boolean;
  /**
   * `on`: registered. `off`: the action is switched off. `needs`: the action is on but the
   * microphone or the screen it needs is not allowed. `taken`: nothing could be registered.
   */
  status: 'on' | 'off' | 'needs' | 'taken';
}

export interface HotkeysView {
  hotkeys: HotkeyRow[];
  /** The switch of dictation. */
  dictation: boolean;
  /** The switch of the question about the screen. */
  screen_ask: boolean;
  /** The user allowed the microphone. */
  microphone: boolean;
  /** The user allowed looking at the screen. */
  screen: boolean;
  /** A speech recognition model is set. */
  stt_model: boolean;
}

/** `KeyboardEvent.code` of a key that is only a modifier. */
const MODIFIER_CODES: ReadonlySet<string> = new Set([
  'ControlLeft',
  'ControlRight',
  'AltLeft',
  'AltRight',
  'ShiftLeft',
  'ShiftRight',
  'MetaLeft',
  'MetaRight',
  'OSLeft',
  'OSRight',
]);

/**
 * The combination a key press stands for: `Ctrl+Alt+KeyD`. Null while only
 * modifiers are down. The key is the browser code as is; Rust tidies it.
 */
export function comboFromEvent(e: { ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean; code: string }): string | null {
  if (!e.code || MODIFIER_CODES.has(e.code)) return null;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Win');
  parts.push(e.code);
  return parts.join('+');
}

/** Where a dictation is: recording, waiting for the text, typing it. */
export type DictationPhase = 'listening' | 'recognising' | 'typing';

export interface DictationView {
  phase: DictationPhase;
  /** Program the text goes to: `winword.exe`. */
  app: string;
}

const PHASES: ReadonlySet<string> = new Set(['listening', 'recognising', 'typing']);

/**
 * The dictation from a `mika://ui` payload. `undefined`: the payload is about
 * something else. `null`: the dictation is over.
 */
export function dictationFrom(payload: unknown): DictationView | null | undefined {
  if (typeof payload !== 'object' || payload === null) return undefined;
  const p = payload as { action?: unknown; phase?: unknown; app?: unknown };
  if (p.action !== 'dictation') return undefined;
  if (typeof p.phase !== 'string' || !PHASES.has(p.phase)) return null;
  return { phase: p.phase as DictationPhase, app: typeof p.app === 'string' ? p.app : '' };
}

/** A picture of a window that waits for the user's question. */
export interface ScreenAsk {
  /** Program of the window: `excel.exe`. */
  app: string;
  /** Title of the window. */
  title: string;
  /** Size of the picture, pixels. */
  width: number;
  height: number;
}

/** Whether a `mika://ui` payload says the waiting picture was thrown away: its card must go. */
export function screenAskGone(payload: unknown): boolean {
  if (typeof payload !== 'object' || payload === null) return false;
  const p = payload as { action?: unknown; gone?: unknown };
  return p.action === 'screen_ask' && p.gone === true;
}

/**
 * Where a new waiting picture leads from what the window shows now. `chat`: the chat
 * comes forward with the card, also from the settings. `drop`: the chat cannot be shown
 * (the welcome screen, or the setup of a provider is still ahead), so the picture is
 * not kept waiting for a card nobody sees.
 */
export function screenAskLands(view: string, needsSetup: boolean): 'chat' | 'drop' {
  if (view === 'welcome') return 'drop';
  if (needsSetup && view !== 'chat') return 'drop';
  return 'chat';
}

/**
 * Her line about a hotkey action (a refusal of dictation or of the screen question),
 * or null. It answers a key press, so the island shows it at once.
 */
export function hotkeyNoticeOf(payload: unknown): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const p = payload as { type?: unknown; kind?: unknown; text?: unknown };
  if (p.type !== 'remark' || (p.kind !== 'dictation' && p.kind !== 'screen_ask')) return null;
  return typeof p.text === 'string' && p.text.trim() ? p.text : null;
}

/**
 * Whether the island shows her line now. An ordinary line waits for the idle state.
 * A line about a hotkey action is also shown while she thinks or speaks: a "wait"
 * said after the answer has ended would come too late to be understood.
 */
export function showsNotice(state: string, notice: string, urgent: boolean): boolean {
  if (!notice) return false;
  if (state === 'idle') return true;
  return urgent && (state === 'thinking' || state === 'speaking');
}

/** The waiting picture from a `mika://ui` payload, or null. */
export function screenAskFrom(payload: unknown): ScreenAsk | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const p = payload as { action?: unknown; app?: unknown; title?: unknown; width?: unknown; height?: unknown };
  if (p.action !== 'screen_ask' || typeof p.title !== 'string') return null;
  return {
    app: typeof p.app === 'string' ? p.app : '',
    title: p.title,
    width: typeof p.width === 'number' ? p.width : 0,
    height: typeof p.height === 'number' ? p.height : 0,
  };
}
