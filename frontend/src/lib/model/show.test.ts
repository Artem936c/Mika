import { describe, expect, it } from 'vitest';
import { EMOTION_ORDER, type Emotion } from '../avatar';
import type { Profile, Slot } from './layers';
import { FULL_STILL, pickShow, sceneOutlived, stillName, stillNames, stillsKey, windowLabel } from './show';

function profile(): Profile {
  const slots = Object.fromEntries(EMOTION_ORDER.map((e) => [e, { source: 'auto', mix: [['happy', 1]] } as Slot])) as Record<Emotion, Slot>;
  return { version: 1, format: 'vrm', spec: '1.0', title: 'T', author: 'A', license: '', expressions: ['happy', 'sad'], slots, lip: 1 };
}

const none = { modelId: '', hasStills: false, failed: false };
const fresh = { modelId: '0123456789abcdef', hasStills: false, failed: false };
const ready = { ...fresh, hasStills: true };

describe('pickShow', () => {
  it('without a model she is drawn from pictures', () => {
    expect(pickShow('full', none)).toBe('pictures');
    expect(pickShow('head', none)).toBe('pictures');
    // Pictures of a model that is not chosen any more are not shown.
    expect(pickShow('head', { ...none, hasStills: true })).toBe('pictures');
  });

  it('a model is alive in the full view and a portrait in a chip', () => {
    expect(pickShow('full', ready)).toBe('live');
    expect(pickShow('full', fresh)).toBe('live');
    expect(pickShow('head', ready)).toBe('portrait');
    expect(pickShow('head', fresh)).toBe('pictures');
  });

  it('a failed scene falls back to pictures in every view', () => {
    for (const s of [fresh, ready]) {
      expect(pickShow('full', { ...s, failed: true })).toBe('pictures');
      expect(pickShow('head', { ...s, failed: true })).toBe('pictures');
    }
  });
});

describe('pictures of a model', () => {
  it('portraits are named by the emotion and the mouth', () => {
    expect(stillName('joy', false)).toBe('joy');
    expect(stillName('joy', true)).toBe('joy-talk');
    expect(FULL_STILL).toBe('full');
  });

  it('there are twenty-one of them, as Rust expects', () => {
    const names = stillNames();
    expect(names).toHaveLength(21);
    expect(names.slice(0, 4)).toEqual(['neutral', 'neutral-talk', 'joy', 'joy-talk']);
    expect(names[20]).toBe('full');
    expect(new Set(names).size).toBe(21);
  });
});

describe('stillsKey', () => {
  it('the same faces give the same key, whatever object carries them', () => {
    expect(stillsKey(profile())).toBe(stillsKey(structuredClone(profile())));
    const renamed = { ...profile(), title: 'Other', author: 'Else' };
    expect(stillsKey(renamed)).toBe(stillsKey(profile()));
  });

  it('a changed slot or lip strength gives another key: the pictures are out of date', () => {
    const base = stillsKey(profile());
    const slot = profile();
    slot.slots.joy = { source: 'manual', mix: [['sad', 1]] };
    expect(stillsKey(slot)).not.toBe(base);
    const gaze = profile();
    gaze.slots.shy = { source: 'auto', mix: [['happy', 1]], gaze: [15, -15] };
    expect(stillsKey(gaze)).not.toBe(base);
    expect(stillsKey({ ...profile(), lip: 0.5 })).not.toBe(base);
  });
});

describe('sceneOutlived', () => {
  it('a scene is let go when its model is no longer the chosen one', () => {
    expect(sceneOutlived('0123456789abcdef', '')).toBe(true);
    expect(sceneOutlived('0123456789abcdef', 'fedcba9876543210')).toBe(true);
  });

  it('the same model keeps its scene, and no model has none to let go', () => {
    expect(sceneOutlived('0123456789abcdef', '0123456789abcdef')).toBe(false);
    expect(sceneOutlived('', '0123456789abcdef')).toBe(false);
    expect(sceneOutlived('', '')).toBe(false);
  });
});

describe('windowLabel', () => {
  it('the avatar window knows its label', () => {
    expect(windowLabel('?view=avatar')).toBe('avatar');
    expect(windowLabel('')).toBe('island');
    expect(windowLabel('?mock=chat')).toBe('island');
    expect(windowLabel('?mock=avatar&view=avatar')).toBe('avatar');
  });
});
