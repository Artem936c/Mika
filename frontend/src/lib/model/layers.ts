// A model avatar, layer by layer: breathing and blinking, the gaze, the lips,
// the emotion, a one-off gesture. Pure functions, no scene and no DOM: the
// scene asks them what to show at a given moment.

import type { Emotion } from '../avatar';

/** How a model shows one emotion; comes from its profile. */
export interface Slot {
  source: 'auto' | 'manual';
  /** Expression name and weight 0..1. */
  mix: [string, number][];
  /** Yaw and pitch of the gaze, degrees; positive is to her left and up. */
  gaze?: [number, number];
}

/** The profile of a model as Rust keeps it beside the model file. */
export interface Profile {
  version: number;
  format: string;
  spec: '0.x' | '1.0';
  title: string;
  author: string;
  license: string;
  expressions: string[];
  slots: Record<Emotion, Slot>;
  /** Lip strength, 0..1.5. */
  lip: number;
}

/** Frames a second, at most. */
export const FPS = 30;
/** Device pixels per CSS pixel, at most. */
export const MAX_PIXEL_RATIO = 2;
/** How long an emotion takes to come and to go, ms. */
export const EMOTION_BLEND_MS = 250;
/** How fast the lips follow their target, ms. */
export const LIP_TAU_MS = 60;
/** Mouth shapes of the VRM format. */
export const VOWELS = ['aa', 'ih', 'ou', 'ee', 'oh'] as const;
export type Vowel = (typeof VOWELS)[number];

/** The arms of a model without animation stand out sideways: how far they come down, degrees. */
export const REST_POSE = { upperArm: 70, lowerArm: 10 } as const;

const clamp01 = (v: number) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);

/** Weight of the blink expression `ms` after a blink began: up in 50, held 90, down in 50. */
export function blinkWeight(ms: number): number {
  if (ms <= 0 || ms >= 190) return 0;
  if (ms < 50) return ms / 50;
  if (ms <= 140) return 1;
  return (190 - ms) / 50;
}

/** False when the slot itself closes the eyes: blink, blinkLeft or blinkRight at 0.5 or more. */
export function canModelBlink(slot: Slot): boolean {
  return !slot.mix.some(([name, weight]) => (name === 'blink' || name === 'blinkLeft' || name === 'blinkRight') && weight >= 0.5);
}

/** Next mouth shape; `null` is a closed mouth; never the previous one. `r` is a random number in [0, 1). */
export function nextVowel(prev: Vowel | null, r: number): Vowel | null {
  const shapes = ([null, ...VOWELS] as (Vowel | null)[]).filter((s) => s !== prev);
  return shapes[Math.min(shapes.length - 1, Math.max(0, Math.floor(r * shapes.length)))];
}

/** Lip gain: the profile strength, times 0.6 under any emotion but the calm one. */
export function lipGain(profile: Profile, emotion: Emotion): number {
  return profile.lip * (emotion === 'neutral' ? 1 : 0.6);
}

/** One step of exponential smoothing towards `target`. */
export function approach(current: number, target: number, dtMs: number, tauMs: number): number {
  if (tauMs <= 0) return target;
  return target + (current - target) * Math.exp(-Math.max(0, dtMs) / tauMs);
}

/** Target weights of a slot, without names the model lacks. */
export function slotWeights(slot: Slot, available: ReadonlySet<string>): Map<string, number> {
  const out = new Map<string, number>();
  for (const [name, weight] of slot.mix) if (available.has(name)) out.set(name, clamp01(weight));
  return out;
}

export interface Pose {
  /** Head, degrees: down is positive. */
  pitch: number;
  /** Head, degrees: to her left is positive. */
  yaw: number;
  /** Head, degrees: towards her left shoulder is positive. */
  roll: number;
  /** Spine, degrees: forward is positive. */
  spine: number;
}

const POSES: Record<Emotion, Pose> = {
  neutral: { pitch: 0, yaw: 0, roll: 0, spine: 0 },
  joy: { pitch: -4, yaw: 0, roll: 4, spine: 0 },
  surprise: { pitch: -6, yaw: 0, roll: 0, spine: -3 },
  angry: { pitch: 6, yaw: 0, roll: 0, spine: 3 },
  sad: { pitch: 12, yaw: 0, roll: 0, spine: 4 },
  shy: { pitch: 8, yaw: 12, roll: -6, spine: 0 },
  think: { pitch: -6, yaw: -12, roll: 8, spine: 0 },
  wink: { pitch: 0, yaw: 0, roll: 8, spine: 0 },
  sleepy: { pitch: 14, yaw: 0, roll: 4, spine: 2 },
  worry: { pitch: 4, yaw: 6, roll: 0, spine: 1 },
};

/** Head and spine angles of a slot. The same for every model. */
export function slotPose(emotion: Emotion): Pose {
  return { ...POSES[emotion] };
}

/**
 * The sign of turns about X and Z on the rig of a model. A VRM 0.x model
 * faces the other way in its own space (the library turns the whole scene
 * round to show it), so what tips a VRM 1.0 head down tips a 0.x head up,
 * and the same for the arms. Turns about Y are the same for both.
 * `metaVersion` is `'0'` or `'1'`, as the library names it.
 */
export function rigSign(metaVersion: string | undefined): 1 | -1 {
  return metaVersion === '0' ? -1 : 1;
}

/** Turns of the bones of the rig, degrees. `head` is about X, Y and Z. */
export interface BoneAngles {
  head: [number, number, number];
  spineX: number;
  chestX: number;
  hipsZ: number;
  leftUpperArmZ: number;
  rightUpperArmZ: number;
  leftLowerArmY: number;
  rightLowerArmY: number;
}

/**
 * Every turn the scene gives the bones: the pose of the emotion, breathing
 * (degrees of the chest), the roll of a one-off gesture and the arms at rest.
 * `sign` is `rigSign` of the model.
 */
export function boneAngles(pose: Pose, breathing: number, roll: number, sign: 1 | -1): BoneAngles {
  return {
    head: [sign * pose.pitch, pose.yaw, -sign * pose.roll],
    spineX: sign * pose.spine,
    chestX: sign * breathing,
    hipsZ: -sign * roll,
    leftUpperArmZ: -sign * REST_POSE.upperArm,
    rightUpperArmZ: sign * REST_POSE.upperArm,
    leftLowerArmY: -REST_POSE.lowerArm,
    rightLowerArmY: REST_POSE.lowerArm,
  };
}

/** Chest pitch of breathing at `ms`, degrees: period 4000 ms, amplitude 1.5. */
export function breath(ms: number): number {
  return 1.5 * Math.sin((2 * Math.PI * ms) / 4000);
}

/** A glance aside: where and for how long. `r1`, `r2` are random numbers in [0, 1). */
export function gazeWander(r1: number, r2: number): { yaw: number; pitch: number; ms: number } {
  return { yaw: (r1 * 2 - 1) * 10, pitch: (r2 * 2 - 1) * 4, ms: 1200 + r2 * 1000 };
}

/** A one-off movement: evenly spaced steps of height (share of her height, up is positive) and roll (degrees). */
interface Move {
  y: number[];
  roll: number[];
  ms: number;
}

// The same movements as the built-in pictures make (`MOTIONS` in avatar.ts):
// one pixel there is 0.0025 of her height here, a sideways shake is a small roll.
const MOVES: Partial<Record<Emotion | 'poke', Move>> = {
  poke: { y: [0, 0.03, 0], roll: [0, 0, 0], ms: 450 },
  joy: { y: [0, 0.035, 0, 0.015, 0], roll: [0, 0, 0, 0, 0], ms: 700 },
  surprise: { y: [0, 0.025, -0.005, 0], roll: [0, 0, 0, 0], ms: 500 },
  angry: { y: [0, 0, 0, 0, 0, 0, 0], roll: [0, -1.8, 1.8, -1.5, 1.5, -0.9, 0], ms: 450 },
  sad: { y: [0, -0.02, -0.015, 0], roll: [0, -1.5, -1, 0], ms: 1600 },
  shy: { y: [0, -0.005, -0.005, 0], roll: [0, -3, -2.5, 0], ms: 1300 },
  wink: { y: [0, 0.0125, 0], roll: [0, 2.5, 0], ms: 500 },
  think: { y: [0, 0, 0, 0], roll: [0, 2.5, 2, 0], ms: 1800 },
  sleepy: { y: [0, -0.015, -0.0025, -0.0175, 0], roll: [0, 2, 0.5, 2.5, 0], ms: 2400 },
  worry: { y: [0, 0, 0, 0, 0, 0], roll: [0, -0.6, 0.6, -0.6, 0.6, 0], ms: 600 },
};

function along(steps: number[], share: number): number {
  const at = share * (steps.length - 1);
  const i = Math.min(steps.length - 2, Math.floor(at));
  const t = at - i;
  // Ease in and out between two steps.
  const eased = t * t * (3 - 2 * t);
  return steps[i] + (steps[i + 1] - steps[i]) * eased;
}

/** Vertical offset and roll of a one-off gesture at `ms`; null when it is over or there is none. */
export function gestureAt(kind: Emotion | 'poke', ms: number): { y: number; roll: number } | null {
  const move = MOVES[kind];
  if (!move || ms < 0 || ms > move.ms) return null;
  const share = ms / move.ms;
  return { y: along(move.y, share), roll: along(move.roll, share) };
}

/** The smallest height the camera shows, metres: a model with odd bones still gets a frame. */
const MIN_FRAME = 0.2;

/**
 * The part of her the camera shows: centre height and visible height, metres.
 * `headY` and `hipsY` are the heights of those bones, `topY` is the top of the head.
 */
export function frameFor(view: 'full' | 'head', headY: number, hipsY: number, topY: number): { centerY: number; height: number } {
  if (view === 'head') {
    // The head bone sits at the neck; the head is what is between it and the top.
    const headSize = Math.max(0, topY - headY);
    return { centerY: headY + 0.45 * headSize, height: Math.max(MIN_FRAME, 1.7 * headSize) };
  }
  // The same cut as the built-in pictures: down to the middle of the thigh,
  // and air above the head, because the places that show her clip the top.
  const bottom = hipsY - THIGH_SHARE * Math.max(0, topY - hipsY);
  const height = Math.max(MIN_FRAME, (topY - bottom) / (1 - HEADROOM));
  return { centerY: bottom + height / 2, height };
}

/** How far below the hips the full view goes, as a share of the hips-to-top height. */
const THIGH_SHARE = 0.3;
/** The share of the full frame left empty above her head. */
const HEADROOM = 0.12;

/** Whether to draw a frame now. */
export function shouldRender(s: { attached: boolean; windowShown: boolean; documentHidden: boolean }): boolean {
  return s.attached && s.windowShown && !s.documentHidden;
}

/** Whether enough time has passed for the next frame at `FPS`. A frame of the screen a hair early still counts. */
export function frameDue(nowMs: number, lastMs: number): boolean {
  return nowMs - lastMs >= 1000 / FPS - 4;
}
