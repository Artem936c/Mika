// The avatar model of this window: which one is chosen, its profile and
// pictures, whether its scene failed here, whether the window is seen.
// One per window: the island and the desktop avatar each load their own.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Profile } from './layers';
import { sceneOutlived, stillNames, windowLabel } from './show';

/** A model as Rust lists it (`ModelView` in avatar_model.rs). */
export interface ModelInfo {
  id: string;
  title: string;
  author: string;
  license: string;
  spec: '0.x' | '1.0';
  size_bytes: number;
  has_stills: boolean;
}

export interface AvatarModels {
  active: string;
  models: ModelInfo[];
}

interface Still {
  name: string;
  png_base64: string;
}

let list = $state<ModelInfo[]>([]);
let id = $state('');
let profile = $state<Profile | null>(null);
let stills = $state<ReadonlyMap<string, string>>(new Map());
let failed = $state('');
let stillsFailed = $state(false);
let windowShown = $state(true);

/** How a model looks and moves, from the persona page of the settings (`AvatarLook` in Rust). */
export interface Look {
  pose: string;
  scale: number;
  frame: 'full' | 'waist' | 'close';
  follow_cursor: boolean;
  idle_actions: boolean;
  /** The user's animation she loops at rest; empty: the rest pose. */
  idle_motion: string;
}

export const DEFAULT_LOOK: Look = {
  pose: 'relaxed',
  scale: 1,
  frame: 'full',
  follow_cursor: true,
  idle_actions: true,
  idle_motion: '',
};
let look = $state<Look>({ ...DEFAULT_LOOK });

export const model = {
  /** Every model the user brought in. */
  get list() {
    return list;
  },
  /** Empty is the built-in avatar. */
  get id() {
    return id;
  },
  get profile() {
    return profile;
  },
  /** Picture name to data URL. */
  get stills() {
    return stills;
  },
  /** Every picture of the model is there. */
  get stillsComplete() {
    return stillNames().every((name) => stills.has(name));
  },
  /** The pictures of the model could not be drawn or saved. */
  get stillsFailed() {
    return stillsFailed;
  },
  /** Why the scene is not shown in this window; empty when it is. */
  get failed() {
    return failed;
  },
  get windowShown() {
    return windowShown;
  },
  get look() {
    return look;
  },
};

/** The scene could not be shown in this window: it goes back to the pictures. */
export function reportFailure(reason: string) {
  failed = reason || 'unknown';
}

/** The pictures for the chips did not come out: the chips keep what they had. */
export function reportStillsFailure() {
  stillsFailed = true;
}

/** Set by the scene module once it is loaded: lets the scene of this window go. */
let dropScene: (() => void) | null = null;

export function onSceneDrop(drop: () => void) {
  dropScene = drop;
}

let turn = 0;

/** Reads the chosen model again. A newer call wins over a slower earlier one. */
export async function reloadModel(): Promise<void> {
  const mine = ++turn;
  const all = await invoke<AvatarModels>('avatar_models');
  const next = all.active;
  if (next === '') {
    if (mine !== turn) return;
    if (sceneOutlived(id, '')) dropScene?.();
    list = all.models;
    stillsFailed = false;
    id = '';
    profile = null;
    stills = new Map();
    failed = '';
    return;
  }
  const [nextProfile, pictures] = await Promise.all([
    invoke<Profile>('avatar_model_profile', { id: next }),
    invoke<Still[]>('avatar_model_stills', { id: next }),
  ]);
  if (mine !== turn) return;
  if (sceneOutlived(id, next)) dropScene?.();
  list = all.models;
  // Another model gets its own try at the pictures.
  if (next !== id) stillsFailed = false;
  id = next;
  profile = nextProfile;
  stills = new Map(pictures.map((s) => [s.name, `data:image/png;base64,${s.png_base64}`]));
  // Another model or another profile: the scene gets one more try.
  failed = '';
}

/** Reads the look of the model again. */
export async function reloadLook(): Promise<void> {
  try {
    look = { ...DEFAULT_LOOK, ...(await invoke<Look>('avatar_look')) };
  } catch {
    // No such command here: the look stays as it is.
  }
}

let started = false;

/** Called once by the root of a window. Outside Tauri and without a mock she stays built-in. */
export async function startModelState(): Promise<void> {
  if (started) return;
  started = true;
  const label = windowLabel(location.search);
  try {
    await listen<{ action?: string; label?: string; shown?: boolean }>('mika://ui', (e) => {
      const p = e.payload;
      if (p?.action === 'avatar_model_changed') reloadModel().catch(() => {});
      if (p?.action === 'avatar_look_changed') reloadLook();
      if (p?.action === 'window_shown' && p.label === label) windowShown = p.shown !== false;
    });
    await reloadLook();
    await reloadModel();
  } catch {
    // No model commands here: the built-in avatar is shown.
  }
}
