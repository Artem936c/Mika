// A click on the living model (decision 0040): which part of her it hit and
// how her body answers. The face she makes comes from the emotion core
// (`poke_avatar` with the part). Chest, hips and legs are never told apart.
// Pure functions, no DOM.

import type { Timeline } from './body';
import { mirror, type RigPose } from './rig';

export type TouchPart = 'crown' | 'face' | 'hand' | 'other';

/** The part of her for the nearest bone; `up` is how high on the head the click is, 0 at the neck, 1 at the top. */
export function partOf(bone: string, up: number): TouchPart {
  if (bone === 'head') return up > 0.55 ? 'crown' : 'face';
  if (/Arm|Hand|Thumb|Index|Middle|Ring|Little/.test(bone)) return 'hand';
  return 'other';
}

/** The bone nearest to a point, all in metres of the scene. */
export function nearestBone(point: [number, number, number], bones: Record<string, [number, number, number]>): string {
  let best = '';
  let bestDist = Infinity;
  for (const [name, [x, y, z]] of Object.entries(bones)) {
    const d = (x - point[0]) ** 2 + (y - point[1]) ** 2 + (z - point[2]) ** 2;
    if (d < bestDist) {
      bestDist = d;
      best = name;
    }
  }
  return best;
}

/** A pixel at least this opaque (0..255) is her; anything clearer lets the click through. */
const OPAQUE = 32;

/** Whether the pixel under the cursor, of this opacity, is her. */
export function isOnFigure(alpha: number): boolean {
  return alpha >= OPAQUE;
}

/** Her right hand raised beside her head, the palm to you, the hand at `side` degrees. */
const wave = (side: number): RigPose =>
  mirror({
    leftUpperArm: [0, -30, -40],
    leftLowerArm: [-90, -120, 0],
    leftHand: [0, 0, side],
  });

/** A wave of the right hand, back and forth twice. */
export const WAVE: Timeline = {
  ms: 1600,
  keys: [wave(-18), wave(18), wave(-18), wave(18), wave(0)],
};

/** How her body answers a click on each part; nothing special for the rest. */
export const REACTION: Record<TouchPart, Timeline | null> = {
  // She leans into the hand that pats her.
  crown: {
    ms: 1400,
    keys: [{ head: [-6, 0, 8] }, { head: [-4, 0, 10] }, { head: [-6, 0, 8] }],
  },
  // She looks away and down.
  face: {
    ms: 1400,
    keys: [
      { head: [10, -22, -6], spine: [0, -6, 0] },
      { head: [12, -24, -6], spine: [0, -6, 0] },
    ],
  },
  // She waves back.
  hand: WAVE,
  other: null,
};

/**
 * The opacity under the cursor is read from a frame as it is drawn. The page
 * asks for a point; the scene reads it once after its next frame and answers.
 * Nothing is read while nobody asks.
 */
export class Probe {
  private asked: { x: number; y: number; answer: (alpha: number) => void } | null = null;

  /** Asks for the opacity at a point; a later ask replaces an earlier one. */
  ask(x: number, y: number, answer: (alpha: number) => void) {
    this.asked = { x, y, answer };
  }

  /** Reads the asked point with `read` and answers; false when nothing was asked. */
  take(read: (x: number, y: number) => number): boolean {
    const a = this.asked;
    if (!a) return false;
    this.asked = null;
    a.answer(read(a.x, a.y));
    return true;
  }
}

/** The size steps at most this often under the wheel, ms: a touchpad sends a flood. */
export const WHEEL_GAP_MS = 150;

/** Whether a wheel event at `now` makes a step after the last step at `lastAt`. */
export function wheelDue(lastAt: number, now: number): boolean {
  return now - lastAt >= WHEEL_GAP_MS;
}
