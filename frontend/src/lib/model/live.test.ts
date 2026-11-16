import { describe, expect, it } from 'vitest';
import { cursorWanted, locate, releaseShownScene, setShownScene } from './live.svelte';
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

describe('the cursor of the window', () => {
  it('is asked for while any view wants it and given up when the last one leaves', () => {
    expect(cursorWanted(+1)).toBe(true);
    expect(cursorWanted(+1)).toBe(null);
    expect(cursorWanted(-1)).toBe(null);
    expect(cursorWanted(-1)).toBe(false);
    // Never below none.
    expect(cursorWanted(-1)).toBe(null);
  });
});
