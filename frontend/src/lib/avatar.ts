// Built-in avatar: generated anime illustrations, one frame per emotion, plus eye and
// mouth layers for blinking and talking (decision 0013). Pure functions, no DOM.
// A stand-in until the Live2D model of epic 7.

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
export const EYE_LAYERS = ['eyes-half', 'eyes-shut'] as const;
export const MOUTH_LAYERS = ['mouth-ajar', 'mouth-oo', 'mouth-shut'] as const;
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

/** Emotions whose mouth sits where the neutral one does, so the mouth layers fit them. */
export function canTalk(e: Emotion): boolean {
  return e === 'neutral' || e === 'joy' || e === 'wink';
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

/**
 * Next mouth shape while she talks. `null` is the open mouth of the frame itself.
 * Never repeats the previous shape, so the lips keep moving.
 */
export function nextMouth(prev: MouthLayer | null, r: number): MouthLayer | null {
  const shapes: (MouthLayer | null)[] = [null, 'mouth-ajar', 'mouth-oo', 'mouth-shut'].filter(
    (s) => s !== prev,
  ) as (MouthLayer | null)[];
  return shapes[Math.min(shapes.length - 1, Math.floor(r * shapes.length))];
}

/** How often the mouth changes while she talks, ms. */
export const MOUTH_STEP_MS = 110;
