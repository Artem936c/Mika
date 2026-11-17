// Her body over time (decision 0040): the slow life of standing still, the
// body of each emotion and of each state of hers, short movements played
// from keys, and the hands that go with her speech. Our own angles for a
// VRM 1.0 rig, as in poses.ts. Pure functions, no DOM.

import type { Emotion } from '../avatar';
import { REST_POSES, REST_POSE_NAMES, type RestPose } from './poses';
import { both, mirror, type RigPose } from './rig';

/** A movement played once: poses spread evenly over `ms`; it comes in and goes out softly. */
export interface Timeline {
  ms: number;
  keys: RigPose[];
  /** Share of the movement spent coming in, and as much going out; `FADE` when not set. */
  fade?: number;
}

/** The share of a movement spent coming in, and as much going out. */
const FADE = 0.25;

const smooth = (t: number) => t * t * (3 - 2 * t);

/**
 * The pose of a movement `ms` after it began and how much of it shows over
 * the pose underneath: 0 at both ends, 1 in the middle. Null outside it.
 */
export function sampleTimeline(tl: Timeline, ms: number): { pose: RigPose; weight: number } | null {
  if (ms < 0 || ms > tl.ms || tl.keys.length === 0) return null;
  const share = ms / tl.ms;
  const fade = tl.fade ?? FADE;
  const weight = share < fade ? smooth(share / fade) : share > 1 - fade ? smooth((1 - share) / fade) : 1;
  if (tl.keys.length === 1) return { pose: tl.keys[0], weight };
  const at = share * (tl.keys.length - 1);
  const i = Math.min(tl.keys.length - 2, Math.floor(at));
  const t = smooth(at - i);
  const a = tl.keys[i];
  const b = tl.keys[i + 1];
  const pose: RigPose = {};
  for (const bone of new Set([...Object.keys(a), ...Object.keys(b)]) as Set<keyof RigPose>) {
    const from = a[bone] ?? b[bone]!;
    const to = b[bone] ?? a[bone]!;
    pose[bone] = [0, 1, 2].map((k) => from[k] + (to[k] - from[k]) * t + 0) as [number, number, number];
  }
  return { pose, weight };
}

// ---- a window on the move ----

/** The steepest lean of a moving window, degrees. */
const MAX_WINDOW_LEAN = 8;

/**
 * How far her top leans while the window moves at `vx` pixels a second,
 * degrees about Z. Her feet go with the window and her top stays behind: a
 * window moving right tips her to the left of the screen, which is a
 * positive turn for a VRM 1.0 rig that faces the user.
 */
export function windowLean(vx: number): number {
  return Math.max(-MAX_WINDOW_LEAN, Math.min(MAX_WINDOW_LEAN, vx / 150)) + 0;
}

// ---- standing still ----

/** The body follows a new pose in about 400 ms. */
export const BODY_TAU_MS = 400 / 3;

/** Every bone of the trunk and legs at rest: a pose that goes leaves no turn behind. */
export const ZERO_TRUNK: RigPose = {
  hips: [0, 0, 0],
  spine: [0, 0, 0],
  chest: [0, 0, 0],
  upperChest: [0, 0, 0],
  neck: [0, 0, 0],
  head: [0, 0, 0],
  leftUpperLeg: [0, 0, 0],
  rightUpperLeg: [0, 0, 0],
  leftLowerLeg: [0, 0, 0],
  rightLowerLeg: [0, 0, 0],
};

/** The next rest pose of `auto`: any of the five but the one she has. `r` in [0, 1). */
export function nextAutoPose(current: RestPose, r: number): RestPose {
  const others = REST_POSE_NAMES.filter((n) => n !== current);
  return others[Math.min(others.length - 1, Math.floor(r * others.length))];
}

/**
 * Small turns of standing still at `ms`: the weight goes from one foot to
 * the other every 6 to 10 seconds, the body sways, the head never quite
 * stops. `seed` (0..1) makes two windows differ.
 */
export function idleOffset(ms: number, seed: number): RigPose {
  const period = 6000 + seed * 4000;
  const shift = Math.sin((2 * Math.PI * ms) / period);
  const sway = Math.sin((2 * Math.PI * ms) / (period * 1.7) + seed * 3);
  const look = Math.sin((2 * Math.PI * ms) / 2300 + seed * 5);
  const turn = Math.sin((2 * Math.PI * ms) / 3100 + seed * 7);
  return {
    hips: [0, 1.2 * sway, 2.5 * shift],
    spine: [0, -0.6 * sway, -1.6 * shift],
    chest: [0, 0, -0.8 * shift],
    head: [0.8 * look, 1.2 * turn, 0.9 * shift],
    leftUpperLeg: [0, 0, -2.5 * shift],
    rightUpperLeg: [0, 0, -2.5 * shift],
    leftLowerLeg: [3 * Math.max(0, -shift), 0, 0],
    rightLowerLeg: [3 * Math.max(0, shift), 0, 0],
  };
}

/** Pause before the next idle action, ms: 20 to 60 seconds. `r` in [0, 1). */
export function idleActionPause(r: number): number {
  return 20000 + r * 40000;
}

/** Pause before the rest pose changes by itself, ms: 2 to 5 minutes. `r` in [0, 1). */
export function autoPosePause(r: number): number {
  return 120000 + r * 180000;
}

/** A small jump of the eyes and how long they stay there. `r1`, `r2` in [0, 1). */
export function saccade(r1: number, r2: number): { yaw: number; pitch: number; ms: number } {
  return {
    yaw: (r1 * 2 - 1) * 2,
    pitch: (r2 * 2 - 1) * 1,
    ms: 600 + r2 * 2400,
  };
}

/** One blink in ten is followed by another. `r` in [0, 1). */
export function twiceBlink(r: number): boolean {
  return r < 0.1;
}

export type IdleAction = 'hair' | 'stretch' | 'look_around' | 'look_hand';

const hairUp: RigPose = {
  leftShoulder: [0, 0, 8],
  leftUpperArm: [-90, -30, 30],
  leftLowerArm: [0, -150, 0],
  leftHand: [0, 0, -20],
  head: [0, 0, -5],
};

const stretchUp: RigPose = {
  ...both({
    leftShoulder: [0, 0, 10],
    leftUpperArm: [0, 0, 70],
    leftLowerArm: [0, -10, 0],
    leftHand: [0, 0, 10],
  }),
  spine: [-6, 0, 0],
  head: [-8, 0, 0],
};

const handUp: RigPose = {
  leftUpperArm: [-30, -30, -85],
  leftLowerArm: [0, -100, 0],
  leftHand: [-20, 0, -10],
  head: [15, 15, 0],
};

export const IDLE_ACTIONS: Record<IdleAction, Timeline> = {
  hair: {
    ms: 2600,
    keys: [hairUp, { ...hairUp, leftHand: [0, 10, -5] }, hairUp],
  },
  stretch: {
    ms: 3200,
    keys: [stretchUp, { ...stretchUp, spine: [-9, 0, 0] }, stretchUp],
  },
  look_around: {
    ms: 3000,
    keys: [
      { head: [0, 28, 0], spine: [0, 6, 0] },
      { head: [0, -28, 0], spine: [0, -6, 0] },
      { head: [0, 0, 0], spine: [0, 0, 0] },
    ],
  },
  look_hand: {
    ms: 2800,
    keys: [
      handUp,
      {
        ...handUp,
        leftIndexProximal: [0, 0, -40],
        leftMiddleProximal: [0, 0, -40],
      },
      handUp,
    ],
  },
};

// ---- emotion and state ----

const armsOf = (pose: RigPose): RigPose =>
  Object.fromEntries(Object.entries(pose).filter(([bone]) => /Shoulder|Arm|Hand|Thumb|Index|Middle|Ring|Little/.test(bone)));

/**
 * An arm moves as a whole: a body that sets the upper arm, forearm or hand
 * of a side takes the rest of that arm and the fingers from the relaxed
 * pose. Otherwise a forearm or a curled hand of the rest pose would hang on
 * an arm it does not belong to.
 */
function whole(pose: RigPose): RigPose {
  const out: RigPose = { ...pose };
  for (const side of ['left', 'right'] as const) {
    if (!['UpperArm', 'LowerArm', 'Hand'].some((b) => (pose as Record<string, unknown>)[side + b] !== undefined)) continue;
    for (const [bone, angles] of Object.entries(REST_POSES.relaxed)) {
      if (bone.startsWith(side) && !bone.endsWith('Shoulder') && (out as Record<string, unknown>)[bone] === undefined) {
        (out as Record<string, unknown>)[bone] = angles;
      }
    }
  }
  return out;
}

const wholeAll = <K extends string>(bodies: Record<K, RigPose>): Record<K, RigPose> =>
  Object.fromEntries(Object.entries<RigPose>(bodies).map(([k, v]) => [k, whole(v)])) as Record<K, RigPose>;

/** Her right hand at her chin. */
const chinHand: RigPose = mirror({
  leftUpperArm: [-30, -45, -72],
  leftLowerArm: [0, -125, 0],
  leftHand: [0, 0, 15],
});

/** What her body adds to each emotion, over the rest pose. */
export const EMOTION_BODY: Record<Emotion, RigPose> = wholeAll<Emotion>({
  neutral: {},
  // Joy lifts the shoulders; the arms stay as the rest pose holds them.
  joy: both({ leftShoulder: [0, 0, 4] }),
  sad: both({
    leftShoulder: [0, 0, -10],
    leftUpperArm: [-4, 0, -80],
    leftLowerArm: [0, -10, 0],
  }),
  angry: armsOf(REST_POSES.crossed),
  shy: { ...armsOf(REST_POSES.clasped), spine: [0, 8, 0] },
  think: chinHand,
  surprise: {
    ...both({ leftUpperArm: [-15, 0, -55], leftLowerArm: [0, -40, 0] }),
    spine: [-4, 0, 0],
  },
  sleepy: both({ leftShoulder: [0, 0, -6] }),
  worry: both({
    leftUpperArm: [-25, -50, -80],
    leftLowerArm: [0, -120, 0],
    leftHand: [0, 0, -10],
  }),
  wink: armsOf(REST_POSES.hip),
});

export const BODY_STATES = ['thinking', 'working', 'confirm', 'listening'] as const;
export type BodyState = (typeof BODY_STATES)[number];

/** Her body while she is busy with something. */
export const STATE_BODY: Record<BodyState, RigPose> = wholeAll<BodyState>({
  thinking: { ...chinHand, head: [-6, -10, 6] },
  working: {
    ...both({
      leftUpperArm: [-25, -10, -78],
      leftLowerArm: [0, -75, 0],
      leftHand: [0, 0, 0],
    }),
    head: [6, 14, 0],
  },
  confirm: { ...armsOf(REST_POSES.clasped), head: [0, 0, -9] },
  listening: {
    ...mirror({
      leftUpperArm: [-25, -30, -30],
      leftLowerArm: [0, -140, 0],
      leftHand: [0, 0, 20],
    }),
    spine: [6, 0, 0],
    head: [0, 0, 8],
  },
});

/** Which state of the body goes with her state and a tool at work; none when she is free. */
export function bodyState(state: string, working: boolean): BodyState | null {
  if (working) return 'working';
  if (state === 'thinking') return 'thinking';
  if (state === 'awaiting_confirm') return 'confirm';
  if (state === 'listening') return 'listening';
  return null;
}

// ---- speech ----

/** Beats of speech are never closer than this, ms. */
const BEAT_GAP_MS = 600;
/** How long one beat moves a forearm, ms. */
const BEAT_MS = 500;

/** Times of the loud peaks of a phrase (levels 0..255), ms from its start: at most one in `BEAT_GAP_MS`. */
export function beats(levels: number[], frameMs: number): number[] {
  // Levels come scaled to the loudest step of the phrase: a beat is a peak over half of it.
  const floor = 128;
  const out: number[] = [];
  for (let i = 0; i < levels.length; i++) {
    const l = levels[i];
    if (l < floor || l < (levels[i - 1] ?? 0) || l < (levels[i + 1] ?? 0)) continue;
    const at = i * frameMs;
    if (out.length && at - out[out.length - 1] < BEAT_GAP_MS) continue;
    out.push(at);
  }
  return out;
}

/** A forearm and a little nod on a beat, `ms` after it, times `strength`; zero after it. */
export function beatOffset(ms: number, side: 'left' | 'right', strength: number): RigPose {
  const env = ms >= 0 && ms <= BEAT_MS ? Math.sin((Math.PI * ms) / BEAT_MS) * strength : 0;
  const left: RigPose = {
    leftUpperArm: [-5 * env + 0, 0, 3 * env + 0],
    leftLowerArm: [0, -18 * env + 0, 0],
    head: [1.5 * env + 0, 0, 0],
  };
  return side === 'left' ? left : mirror(left);
}

/** How a phrase ends for her head: a nod after `!`, a tilt after `?`. */
export function endCue(text: string): 'nod' | 'tilt' | null {
  const end = text.trim().slice(-1);
  if (end === '!') return 'nod';
  if (end === '?') return 'tilt';
  return null;
}

/** How much her hands move while she speaks with this emotion. */
export function speechStrength(emotion: Emotion): number {
  if (emotion === 'sad' || emotion === 'sleepy') return 0.5;
  if (emotion === 'joy') return 1.3;
  return 1;
}

/** Short movements of the head at the end of a phrase. */
export const END_CUES: Record<'nod' | 'tilt', Timeline> = {
  nod: {
    ms: 600,
    keys: [{ head: [0, 0, 0] }, { head: [9, 0, 0] }, { head: [0, 0, 0] }],
  },
  tilt: { ms: 900, keys: [{ head: [0, 0, 9] }, { head: [-3, 0, 10] }] },
};

// ---- gestures ----

export const GESTURE_NAMES = ['wave', 'bow', 'nod', 'shake_head', 'shrug', 'clap', 'peace', 'spin', 'dance'] as const;
export type GestureName = (typeof GESTURE_NAMES)[number];

/** Her right hand raised beside her head, the palm to you, the hand at `side` degrees. */
const waveArm = (side: number): RigPose =>
  mirror({
    leftUpperArm: [0, -30, -40],
    leftLowerArm: [-90, -120, 0],
    leftHand: [0, 0, side],
  });

/** Both hands in front of her chest, `open` degrees apart at the elbows. */
const clapArms = (open: number): RigPose =>
  both({
    leftUpperArm: [-35, -50 + open, -75],
    leftLowerArm: [0, -100 + open, 0],
    leftHand: [0, 0, 0],
  });

/** Her right hand by her face with two fingers up. */
const peaceHand: RigPose = mirror({
  leftUpperArm: [0, -30, -40],
  leftLowerArm: [-90, -120, 0],
  leftHand: [0, 0, 0],
  leftIndexProximal: [0, -12, 0],
  leftIndexIntermediate: [0, 0, 0],
  leftIndexDistal: [0, 0, 0],
  leftMiddleProximal: [0, 12, 0],
  leftMiddleIntermediate: [0, 0, 0],
  leftMiddleDistal: [0, 0, 0],
  leftRingProximal: [0, 0, -70],
  leftRingIntermediate: [0, 0, -80],
  leftRingDistal: [0, 0, -40],
  leftLittleProximal: [0, 0, -70],
  leftLittleIntermediate: [0, 0, -80],
  leftLittleDistal: [0, 0, -40],
  leftThumbMetacarpal: [0, 30, 10],
  leftThumbProximal: [0, 30, 20],
  leftThumbDistal: [0, 20, 20],
});

/** One beat of her dance: the hips to one side, the arms up on the other. */
const danceBeat = (side: 1 | -1): RigPose => ({
  hips: [0, 8 * side, 6 * side],
  spine: [0, -4 * side, -5 * side],
  head: [0, 6 * side, 6 * side],
  ...both({
    leftUpperArm: [-20, 0, side > 0 ? -20 : -60],
    leftLowerArm: [0, side > 0 ? -90 : -40, 0],
  }),
});

/** Her own gestures, played once when asked (the `avatar_gesture` tool). */
export const GESTURES: Record<GestureName, Timeline> = {
  wave: {
    ms: 1800,
    keys: [waveArm(-18), waveArm(18), waveArm(-18), waveArm(18), waveArm(0)],
  },
  bow: {
    ms: 1800,
    keys: [
      {
        spine: [28, 0, 0],
        chest: [8, 0, 0],
        head: [12, 0, 0],
        ...both({ leftUpperArm: [-10, -45, -84], leftLowerArm: [0, -35, 0] }),
      },
      {
        spine: [30, 0, 0],
        chest: [8, 0, 0],
        head: [14, 0, 0],
        ...both({ leftUpperArm: [-10, -45, -84], leftLowerArm: [0, -35, 0] }),
      },
    ],
  },
  nod: {
    ms: 1100,
    keys: [{ head: [0, 0, 0] }, { head: [12, 0, 0] }, { head: [0, 0, 0] }, { head: [12, 0, 0] }, { head: [0, 0, 0] }],
  },
  shake_head: {
    ms: 1300,
    keys: [{ head: [0, 0, 0] }, { head: [0, 16, 0] }, { head: [0, -16, 0] }, { head: [0, 14, 0] }, { head: [0, 0, 0] }],
  },
  shrug: {
    ms: 1500,
    keys: [
      {
        ...both({
          leftShoulder: [0, 0, 12],
          leftUpperArm: [-10, 0, -62],
          leftLowerArm: [-60, -60, 0],
          leftHand: [0, 0, 20],
        }),
        head: [0, 0, 8],
      },
      {
        ...both({
          leftShoulder: [0, 0, 14],
          leftUpperArm: [-10, 0, -60],
          leftLowerArm: [-60, -62, 0],
          leftHand: [0, 0, 20],
        }),
        head: [0, 0, 9],
      },
    ],
  },
  clap: {
    ms: 1600,
    keys: [clapArms(12), clapArms(0), clapArms(12), clapArms(0), clapArms(12), clapArms(0)],
  },
  peace: {
    ms: 2000,
    keys: [
      { ...peaceHand, head: [0, 0, 8] },
      { ...peaceHand, head: [0, 0, 10] },
    ],
  },
  // A whole turn ends where it began: no slow way out, or she would turn back.
  spin: {
    ms: 1600,
    fade: 0.03,
    keys: [{ hips: [0, 0, 0] }, { hips: [0, 120, 0] }, { hips: [0, 240, 0] }, { hips: [0, 360, 0] }],
  },
  dance: {
    ms: 4000,
    keys: [danceBeat(1), danceBeat(-1), danceBeat(1), danceBeat(-1), danceBeat(1), danceBeat(-1), danceBeat(1)],
  },
};
