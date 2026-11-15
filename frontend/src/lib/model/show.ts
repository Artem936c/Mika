// Which of the three ways draws the avatar, and the names of a model's pictures.
// Pure functions, no DOM.

import { EMOTION_ORDER, type AvatarView, type Emotion } from '../avatar';
import type { Profile } from './layers';

/** The built-in pictures, the living model, or a picture of the model made at import. */
export type Show = 'pictures' | 'live' | 'portrait';

/**
 * `modelId` is empty for the built-in avatar. `hasStills`: the picture this
 * chip needs is there. `failed`: the scene could not be shown in this window.
 */
export function pickShow(view: AvatarView, s: { modelId: string; hasStills: boolean; failed: boolean }): Show {
  if (s.modelId === '' || s.failed) return 'pictures';
  if (view === 'full') return 'live';
  return s.hasStills ? 'portrait' : 'pictures';
}

/** The picture of an emotion with the mouth shut or open. */
export function stillName(emotion: Emotion, mouthOpen: boolean): string {
  return mouthOpen ? `${emotion}-talk` : emotion;
}

/** The whole figure with a calm face: shown while the model loads. */
export const FULL_STILL = 'full';

/** Every picture of a model, in the order Rust keeps them (`still_names()` in mika-core). */
export function stillNames(): string[] {
  return [...EMOTION_ORDER.flatMap((e) => [stillName(e, false), stillName(e, true)]), FULL_STILL];
}

/**
 * What the pictures of a model depend on: its slots and its lip strength.
 * Pictures drawn for one key are out of date for another.
 */
export function stillsKey(profile: Profile): string {
  return JSON.stringify([EMOTION_ORDER.map((e) => [profile.slots[e]?.mix ?? [], profile.slots[e]?.gaze ?? null]), profile.lip]);
}

/**
 * Whether the scene of model `was` has no use any more once model `now` is
 * the chosen one: it holds the model in the memory of the video card.
 */
export function sceneOutlived(was: string, now: string): boolean {
  return was !== '' && was !== now;
}

/** The Tauri label of the window this page runs in, from `location.search`. */
export function windowLabel(search: string): 'island' | 'avatar' {
  return new URLSearchParams(search).get('view') === 'avatar' ? 'avatar' : 'island';
}
