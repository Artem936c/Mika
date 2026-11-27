import { describe, expect, it } from 'vitest';
import { locate, releaseShownScene, setShownScene } from './live.svelte';
import type { Scene } from './scene';

const scene = { touch: () => 'hand' } as unknown as Scene;

describe('the scene of the window', () => {
  it('stays with the view that came in when the view that leaves lets go', () => {
    const chat = {} as HTMLElement;
    const settings = {} as HTMLElement;
    setShownScene(scene, settings);
    setShownScene(scene, chat);
    releaseShownScene(settings);
    expect(locate(0, 0)).toBe('hand');
    releaseShownScene(chat);
    expect(locate(0, 0)).toBe(null);
  });
});
