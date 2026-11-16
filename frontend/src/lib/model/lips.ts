// The lips of a model while the voice sounds (decision 0040). The shape of
// the mouth comes from the letters of the phrase, the opening from its
// loudness, which Rust measures every `frameMs`. Pure functions, no DOM.

import type { Vowel } from './layers';

/** A mouth shape of the format, lips pressed together, or no shape of its own. */
export type Viseme = Vowel | 'closed' | null;

/** One step of a phrase: the shape of the mouth and how far it is open, 0..1. */
export interface MouthFrame {
  shape: Vowel | null;
  open: number;
}

/** The mouth opens this fast towards a louder step, ms. */
export const OPEN_TAU_MS = 30;
/** And closes this slow towards a quieter one, ms. */
export const CLOSE_TAU_MS = 80;

/** A step below this share of the loudest one is a pause. */
const VOICED = 0.15;
/** How far the mouth stays open on m, b and p, as a share. */
const CLOSED_SHARE = 0.15;
/** How far it stays open on other consonants, as a share. */
const CONSONANT_SHARE = 0.6;
/** Opening of a vowel when nothing is measured. */
const FLAT_OPEN = 0.6;

// Cyrillic letters are written as escapes: the sources keep no Cyrillic
// outside the locale files (decision 0035).
const SHAPES: Record<string, Viseme> = {
  '\u0430': 'aa',
  '\u044f': 'aa',
  a: 'aa',
  '\u043e': 'oh',
  o: 'oh',
  '\u0443': 'ou',
  '\u044e': 'ou',
  u: 'ou',
  w: 'ou',
  '\u0438': 'ih',
  '\u044b': 'ih',
  i: 'ih',
  y: 'ih',
  '\u0435': 'ee',
  '\u044d': 'ee',
  '\u0451': 'ee',
  e: 'ee',
  '\u043c': 'closed',
  '\u0431': 'closed',
  '\u043f': 'closed',
  m: 'closed',
  b: 'closed',
  p: 'closed',
};

/** The mouth for one letter; `null` for any letter that does not shape it, and for anything else. */
export function visemeOf(ch: string): Viseme {
  return SHAPES[ch.toLowerCase()] ?? null;
}

/** The letters of a phrase that are said, as mouth shapes. */
function letters(text: string): Viseme[] {
  return [...text].filter((ch) => /\p{L}/u.test(ch)).map(visemeOf);
}

/** The mouth for one letter at an opening, after the shape before it. */
function frameOf(viseme: Viseme, open: number, before: Vowel | null): MouthFrame {
  if (viseme === 'closed') return { shape: before, open: open * CLOSED_SHARE };
  if (viseme === null) return { shape: before, open: open * CONSONANT_SHARE };
  return { shape: viseme, open };
}

/**
 * The mouth for each step of a phrase whose loudness is known: the letters
 * are spread over the sounding steps in their order, pauses fall on the
 * quiet ones. A phrase without letters opens on `aa` by loudness.
 */
export function lipTrack(text: string, levels: number[], _frameMs: number): MouthFrame[] {
  const said = letters(text);
  const loud = levels.map((l) => Math.min(1, Math.max(0, l / 255)));
  const voiced = loud.filter((l) => l >= VOICED).length;
  let k = 0;
  let shape: Vowel | null = null;
  return loud.map((open) => {
    if (open < VOICED) return { shape, open: 0 };
    if (said.length === 0) {
      shape = 'aa';
      return { shape, open };
    }
    const letter = said[Math.min(said.length - 1, Math.floor((k * said.length) / voiced))];
    k += 1;
    const frame = frameOf(letter, open, shape);
    shape = frame.shape;
    return frame;
  });
}

/** The mouth for a phrase whose loudness could not be measured: `msPerChar` for each letter, an even opening. */
export function flatTrack(text: string, msPerChar: number, frameMs: number): MouthFrame[] {
  const said = letters(text);
  if (said.length === 0 || frameMs <= 0) return [];
  const steps = Math.ceil((said.length * msPerChar) / frameMs);
  let shape: Vowel | null = null;
  const out: MouthFrame[] = [];
  for (let k = 0; k < steps; k++) {
    const letter = said[Math.min(said.length - 1, Math.floor((k * frameMs) / msPerChar))];
    const frame = frameOf(letter, FLAT_OPEN, shape);
    shape = frame.shape;
    out.push(frame);
  }
  return out;
}

/** The step of `track` at `now` for a phrase that began at `startedAt`; null outside it. */
export function mouthAt(track: MouthFrame[], startedAt: number, now: number, frameMs: number): MouthFrame | null {
  if (now < startedAt || frameMs <= 0) return null;
  const i = Math.floor((now - startedAt) / frameMs);
  return i < track.length ? track[i] : null;
}

/** One step of the opening towards `target`: quick to open, slower to close. */
export function smoothOpen(current: number, target: number, dtMs: number): number {
  const tau = target > current ? OPEN_TAU_MS : CLOSE_TAU_MS;
  return target + (current - target) * Math.exp(-Math.max(0, dtMs) / tau);
}
