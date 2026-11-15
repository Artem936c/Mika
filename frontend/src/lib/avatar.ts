// Built-in avatar: generated anime illustrations, one frame per emotion, plus eye and
// mouth layers for blinking and talking (decision 0013). Pure functions, no DOM.
// The avatar by default, and what is shown when a model of the user cannot be
// (decision 0039; the model lives in `model/`).

export const EMOTION_ORDER = [
  'neutral',
  'joy',
  'shy',
  'surprise',
  'sad',
  'angry',
  'think',
  'wink',
  'sleepy',
  'worry',
] as const;

export type Emotion = (typeof EMOTION_ORDER)[number];

export function isEmotion(v: unknown): v is Emotion {
  return typeof v === 'string' && (EMOTION_ORDER as readonly string[]).includes(v);
}

/** The emotion after a bus event: only `emotion_changed` moves it. */
export function reduceEmotion(current: Emotion, event: unknown): Emotion {
  if (typeof event !== 'object' || event === null) return current;
  const e = event as { type?: unknown; emotion?: unknown };
  return e.type === 'emotion_changed' && isEmotion(e.emotion) ? e.emotion : current;
}

export type AvatarView = 'head' | 'full';

/** Layers drawn over the emotion frame. All frames share one size and line up pixel for pixel. */
export const EYE_LAYERS = ['eyes-half', 'eyes-shut', 'eyes-left', 'eyes-right'] as const;
/** Mouth shapes drawn from the calm face; they fit the calm, joyful and winking frames. */
const CALM_MOUTHS = ['mouth-ajar', 'mouth-oo', 'mouth-shut'] as const;
/** An open, speaking mouth for each other emotion, drawn from that emotion's own face. */
const TALK_MOUTHS = ['talk-shy', 'talk-surprise', 'talk-sad', 'talk-angry', 'talk-think', 'talk-sleepy', 'talk-worry'] as const;
export const MOUTH_LAYERS = [...CALM_MOUTHS, ...TALK_MOUTHS] as const;
export type EyeLayer = (typeof EYE_LAYERS)[number];
export type MouthLayer = (typeof MOUTH_LAYERS)[number];

const FILES = import.meta.glob('../assets/avatar/*.webp', {
  eager: true,
  query: '?url',
  import: 'default',
}) as Record<string, string>;

/** URL of a frame or layer by name, `''` when the file is missing. */
export function frameUrl(name: Emotion | EyeLayer | MouthLayer): string {
  return FILES[`../assets/avatar/${name}.webp`] ?? '';
}

/** Width to height of every frame. */
export const FRAME_RATIO = 3 / 4;

/**
 * The head chip shows this part of the frame, in fractions of its width and height:
 * from `x`, `y`, `w` wide. The chip is square.
 */
export const HEAD_CROP = { x: 0.2833, y: 0.075, w: 0.4333 } as const;

/** Emotions whose eyes are open in the neutral way, so the blink layers fit them. */
export function canBlink(e: Emotion): boolean {
  return e === 'neutral';
}

/**
 * Mouth shapes she cycles through while talking with this face. `null` is the mouth
 * of the frame itself. Every emotion has at least one other shape.
 */
export function talkShapes(e: Emotion): (MouthLayer | null)[] {
  if (e === 'neutral' || e === 'joy' || e === 'wink') return [null, ...CALM_MOUTHS];
  return [null, `talk-${e}` as MouthLayer];
}

/** One blink: eye layers and how long each stays, ms. `null` is open eyes. */
export const BLINK: readonly [EyeLayer | null, number][] = [
  ['eyes-half', 50],
  ['eyes-shut', 90],
  ['eyes-half', 50],
  [null, 0],
];

/** Pause before the next blink, ms. `r` is a random number in [0, 1). */
export function blinkPause(r: number): number {
  return 2400 + Math.floor(r * 3600);
}

/** Next mouth shape for face `e` while she talks; never the previous one, so the lips keep moving. */
export function nextMouth(e: Emotion, prev: MouthLayer | null, r: number): MouthLayer | null {
  const shapes = talkShapes(e).filter((s) => s !== prev);
  return shapes[Math.min(shapes.length - 1, Math.floor(r * shapes.length))];
}

/** How often the mouth changes while she talks, ms. */
export const MOUTH_STEP_MS = 110;

/** A small idle gesture: a glance aside or a closed-mouth smile, held for `ms`. */
export interface Gesture {
  eyes?: EyeLayer;
  mouth?: MouthLayer;
  ms: number;
}

const GESTURES: Gesture[] = [
  { eyes: 'eyes-left', ms: 1400 },
  { eyes: 'eyes-right', ms: 1400 },
  { mouth: 'mouth-shut', ms: 2200 },
  { eyes: 'eyes-right', mouth: 'mouth-shut', ms: 1800 },
  { eyes: 'eyes-left', mouth: 'mouth-ajar', ms: 1200 },
];

/** Idle gestures fit only the calm face: the layers come from it. */
export function canGesture(e: Emotion): boolean {
  return e === 'neutral';
}

/** The next gesture; `r` is a random number in [0, 1). */
export function idleGesture(r: number): Gesture {
  return GESTURES[Math.min(GESTURES.length - 1, Math.floor(r * GESTURES.length))];
}

/** Pause before the next idle gesture, ms. */
export function gesturePause(r: number): number {
  return 4500 + Math.floor(r * 5500);
}

/** A one-off body movement: Web Animations keyframes of `transform`, and length. */
export interface Motion {
  frames: { transform: string; offset?: number }[];
  ms: number;
}

const t = (y: number, rot = 0, x = 0, s = 1) => `translate(${x}px, ${y}px) rotate(${rot}deg) scale(${s})`;

/** How the body moves when an emotion begins. Neutral needs none. */
export const MOTIONS: Partial<Record<Emotion, Motion>> = {
  joy: { frames: [{ transform: t(0) }, { transform: t(-14, 0, 0, 1.01) }, { transform: t(0) }, { transform: t(-6) }, { transform: t(0) }], ms: 700 },
  surprise: { frames: [{ transform: t(0) }, { transform: t(-10, 0, 0, 1.02) }, { transform: t(2) }, { transform: t(0) }], ms: 500 },
  angry: { frames: [0, -6, 6, -5, 5, -3, 0].map((x) => ({ transform: t(0, 0, x) })), ms: 450 },
  sad: { frames: [{ transform: t(0) }, { transform: t(8, -1.5) }, { transform: t(6, -1) }, { transform: t(0) }], ms: 1600 },
  shy: { frames: [{ transform: t(0) }, { transform: t(2, -3, -4) }, { transform: t(2, -2.5, -3) }, { transform: t(0) }], ms: 1300 },
  wink: { frames: [{ transform: t(0) }, { transform: t(-5, 2.5) }, { transform: t(0) }], ms: 500 },
  think: { frames: [{ transform: t(0) }, { transform: t(0, 2.5, 2) }, { transform: t(0, 2, 2) }, { transform: t(0) }], ms: 1800 },
  sleepy: { frames: [{ transform: t(0) }, { transform: t(6, 2) }, { transform: t(1, 0.5) }, { transform: t(7, 2.5) }, { transform: t(0) }], ms: 2400 },
  worry: { frames: [0, -2, 2, -2, 2, 0].map((x) => ({ transform: t(0, 0, x) })), ms: 600 },
};

/** A hop when she is clicked. */
export const POKE_MOTION: Motion = {
  frames: [{ transform: t(0) }, { transform: t(4, 0, 0, 0.98) }, { transform: t(-12, 0, 0, 1.02) }, { transform: t(0) }],
  ms: 450,
};
