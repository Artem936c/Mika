// The manner of her character (decision 0055): how the chosen character
// carries her head, how lively she stands and talks, what she does at rest,
// and how her body answers a click. The face of a click comes from the
// emotion core, which knows the character too; the body here follows that
// face. Pure data and functions, no DOM.

import type { Emotion } from '../avatar';
import { EMOTION_BODY, GESTURES, STATE_BODY, type IdleAction, type Timeline } from './body';
import { REST_POSES, type RestPose } from './poses';
import type { Euler3, RigPose } from './rig';
import { REACTION, WAVE, type TouchPart } from './touch';

export interface Manner {
  /** How she holds her head at rest, degrees: down, to her left, towards her left shoulder. */
  carriage: Euler3;
  /** How much she sways while standing: 1 is the plain slow life. */
  sway: number;
  /** How much her hands move with her speech: 1 is plain. */
  hands: number;
  /** The idle actions she does now and then. */
  actions: IdleAction[];
  /** The pause between idle actions, times the plain one. */
  actionPause: number;
  /** The rest poses she takes by herself when the pose is `auto`. */
  autoPoses: RestPose[];
  /** Her face while nothing goes on, instead of the plain one; null keeps the plain one. */
  rest: Emotion | null;
}

export const MANNER_IDS = ['caring', 'cheerful', 'calm', 'bold', 'business', 'sarcastic', 'sleepy', 'custom'] as const;
export type MannerId = (typeof MANNER_IDS)[number];

const caring: Manner = {
  carriage: [2, 0, -3],
  sway: 1,
  hands: 1,
  actions: ['hair', 'look_around', 'look_hand'],
  actionPause: 1,
  autoPoses: ['clasped', 'relaxed', 'behind'],
  rest: null,
};

export const MANNERS: Record<MannerId, Manner> = {
  // Soft: the head a little to one side, the hands held before her.
  caring,
  // Lively: the head up and tilted, more sway, bigger hands, more often busy.
  cheerful: {
    carriage: [-2, 0, 4],
    sway: 1.4,
    hands: 1.3,
    actions: ['hair', 'stretch', 'look_around'],
    actionPause: 0.6,
    autoPoses: ['behind', 'relaxed', 'hip'],
    rest: null,
  },
  // Still: little sway, small hands, long pauses.
  calm: {
    carriage: [1, 0, 0],
    sway: 0.7,
    hands: 0.8,
    actions: ['look_around', 'look_hand'],
    actionPause: 1.5,
    autoPoses: ['relaxed', 'clasped'],
    rest: null,
  },
  // The chin up, a hand on the hip or the arms crossed.
  bold: {
    carriage: [-4, 0, 0],
    sway: 1.1,
    hands: 1.2,
    actions: ['stretch', 'look_around', 'hair'],
    actionPause: 1,
    autoPoses: ['hip', 'crossed'],
    rest: null,
  },
  // Upright and composed: hardly any sway, rare idle actions.
  business: {
    carriage: [-1, 0, 0],
    sway: 0.5,
    hands: 0.8,
    actions: ['look_hand', 'look_around'],
    actionPause: 1.6,
    autoPoses: ['clasped', 'behind'],
    rest: null,
  },
  // The head cocked to one side, a hand on the hip.
  sarcastic: {
    carriage: [0, 5, 5],
    sway: 1,
    hands: 1.1,
    actions: ['hair', 'look_hand', 'look_around'],
    actionPause: 1,
    autoPoses: ['hip', 'crossed'],
    rest: null,
  },
  // The head hangs, she barely moves, stretches now and then, and her eyes droop.
  sleepy: {
    carriage: [6, 0, 3],
    sway: 0.6,
    hands: 0.5,
    actions: ['stretch', 'look_around'],
    actionPause: 1.3,
    autoPoses: ['relaxed', 'behind'],
    rest: 'sleepy',
  },
  // An own character is described in words only: she moves as the caring one.
  custom: { ...caring },
};

/** The manner of a character by its id; the caring one for anything else. */
export function mannerOf(id: string | undefined): Manner {
  return (MANNERS as Record<string, Manner>)[id ?? ''] ?? MANNERS.caring;
}

/** Her face: the plain one at rest takes the face of her manner while she is free. */
export function restingEmotion(emotion: Emotion, state: string, manner: Manner): Emotion {
  if (emotion !== 'neutral' || state !== 'idle' || !manner.rest) return emotion;
  return manner.rest;
}

// ---- a click ----

/** The arms of an emotion, without the trunk: a reaction keeps its own head. */
const arms = (pose: RigPose): RigPose =>
  Object.fromEntries(Object.entries(pose).filter(([bone]) => /Shoulder|Arm|Hand|Thumb|Index|Middle|Ring|Little/.test(bone)));

const crossed = arms(REST_POSES.crossed);
const atChest = arms(STATE_BODY.listening);
const startledArms = arms(EMOTION_BODY.surprise);

/** Cross: she turns away with her arms crossed and shakes her head once. */
const POUT: Timeline = {
  ms: 2200,
  keys: [
    { ...crossed, head: [4, 22, -4] },
    { ...crossed, head: [4, 26, -5] },
    { ...crossed, head: [4, 18, -4] },
    { ...crossed, head: [4, 24, -4] },
  ],
};

/** Startled: she leans back, the hands fly up. */
const STARTLE: Timeline = {
  ms: 1300,
  keys: [
    { ...startledArms, spine: [-8, 0, 0], head: [-6, 0, 0] },
    { ...startledArms, spine: [-6, 0, 0], head: [-4, 0, 0] },
  ],
};

/** Shy of a touch: the hands go to her chest, the head down and away. */
const DUCK: Timeline = {
  ms: 1600,
  keys: [
    { ...atChest, head: [12, -10, -4] },
    { ...atChest, head: [12, -14, -5] },
  ],
};

/** Pleased with a touch to the cheek: her head sways from side to side. */
const SWAY: Timeline = {
  ms: 1500,
  keys: [{ head: [-3, 0, 7] }, { head: [-3, 0, -7] }, { head: [-3, 0, 5] }],
};

/** Sleepy: her head slowly sinks to one side. */
const DROWSE: Timeline = {
  ms: 2000,
  keys: [{ head: [8, 0, 8] }, { head: [11, 0, 10] }],
};

/** Worried: the hands at her chest, the head down. */
const HUDDLE: Timeline = {
  ms: 1600,
  keys: [
    { ...atChest, head: [8, 6, 0] },
    { ...atChest, head: [9, 8, 0] },
  ],
};

/** Sad: the head sinks. */
const DROOP: Timeline = {
  ms: 1600,
  keys: [{ head: [12, 0, 0] }, { head: [13, 0, 2] }],
};

/**
 * How her body answers a click on `part` once her face is `emotion`. The
 * face decides first: cross, startled, playful, sleepy, worried or sad look
 * the same wherever the click was; the others answer the part.
 */
export function reactionFor(part: TouchPart, emotion: Emotion): Timeline | null {
  switch (emotion) {
    case 'angry':
      return POUT;
    case 'surprise':
      return STARTLE;
    case 'wink':
      return GESTURES.peace;
    case 'sleepy':
      return DROWSE;
    case 'worry':
      return HUDDLE;
    case 'sad':
      return DROOP;
    case 'shy':
      if (part === 'face') return REACTION.face;
      if (part === 'hand') return WAVE;
      return DUCK;
    case 'joy':
      if (part === 'face') return SWAY;
      if (part === 'other') return SWAY;
      return part === 'hand' ? WAVE : REACTION.crown;
    default:
      if (part === 'other') return STARTLE;
      return REACTION[part];
  }
}
