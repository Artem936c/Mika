// What the voice is saying right now, for the lips of the avatar (decision
// 0040). Rust tells the window when a phrase starts to sound, with its
// loudness every few milliseconds, and when it falls quiet. One per window.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { flatTrack, lipTrack, mouthAt, type MouthFrame } from './lips';

/** How long one letter sounds when the loudness is not known, ms. */
const MS_PER_CHAR = 65;
/** A phrase whose end never came is let go this long after its last step, ms. */
const LOST_QUIET_MS = 1500;

interface Phrase {
  text: string;
  frameMs: number;
  /** Loudness every `frameMs`, 0..255; empty when unknown. */
  levels: number[];
  track: MouthFrame[];
  /** No loudness came: the track is an estimate and runs in a loop. */
  flat: boolean;
  /** `performance.now()` when it began to sound. */
  startedAt: number;
}

let phrase = $state.raw<Phrase | null>(null);
let speaks = $state(true);

export const voice = {
  /** A phrase of the voice sounds now. */
  get playing() {
    return phrase !== null;
  },
  /** Replies are read aloud: without that the lips follow the text as it is written. */
  get speaks() {
    return speaks;
  },
};

/**
 * Whether her mouth moves now. With replies read aloud it moves while the
 * voice sounds; without, while a reply is written.
 */
export function talkingNow(writing: boolean): boolean {
  return phrase !== null || (writing && !speaks);
}

/** The mouth of this moment while a phrase sounds; null when none does. */
export function mouthNow(now: number): MouthFrame | null {
  const p = phrase;
  if (!p) return null;
  const length = p.track.length * p.frameMs;
  if (p.flat) {
    if (length === 0) return { shape: null, open: 0 };
    // The estimate runs again until the voice says it is quiet.
    return mouthAt(p.track, p.startedAt, p.startedAt + ((now - p.startedAt) % length), p.frameMs);
  }
  if (now > p.startedAt + length + LOST_QUIET_MS) {
    phrase = null;
    return null;
  }
  return mouthAt(p.track, p.startedAt, now, p.frameMs) ?? { shape: null, open: 0 };
}

/** The phrase that sounds now, for the hands that go with it; null in silence. */
export function currentPhrase(): Readonly<Phrase> | null {
  return phrase;
}

/** Takes a `voice_phrase` or `voice_quiet` event; other events are left alone. */
export function onVoiceEvent(payload: unknown, now = performance.now()) {
  const e = payload as {
    type?: string;
    text?: unknown;
    frame_ms?: unknown;
    levels?: unknown;
  };
  if (e?.type === 'voice_quiet') {
    phrase = null;
    return;
  }
  if (e?.type !== 'voice_phrase') return;
  const text = typeof e.text === 'string' ? e.text : '';
  const frameMs = typeof e.frame_ms === 'number' && e.frame_ms > 0 ? e.frame_ms : 20;
  const levels = Array.isArray(e.levels) ? e.levels.filter((l): l is number => typeof l === 'number') : [];
  const flat = levels.length === 0;
  phrase = {
    text,
    frameMs,
    levels,
    track: flat ? flatTrack(text, MS_PER_CHAR, frameMs) : lipTrack(text, levels, frameMs),
    flat,
    startedAt: now,
  };
}

async function readSpeaks() {
  try {
    const s = await invoke<{ speak_replies?: boolean }>('get_ui_settings');
    speaks = s.speak_replies !== false;
  } catch {
    // No settings here: the voice is taken to be on.
  }
}

let started = false;

/** Called once by the root of a window. */
export async function startVoice(): Promise<void> {
  if (started) return;
  started = true;
  try {
    await listen('mika://event', (e) => onVoiceEvent(e.payload));
    await listen<{ action?: string }>('mika://ui', (e) => {
      if (e.payload?.action === 'settings_changed') readSpeaks();
    });
    await readSpeaks();
  } catch {
    // Outside Tauri and without a mock nothing sounds.
  }
}
