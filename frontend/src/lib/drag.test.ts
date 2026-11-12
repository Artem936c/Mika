import { describe, expect, it } from 'vitest';
import { DRAG_THRESHOLD, EDGES, isDrag } from './drag';

describe('drag', () => {
  it('a short press is a click, a longer move is a drag', () => {
    expect(isDrag(0, 0)).toBe(false);
    expect(isDrag(3, 4)).toBe(false);
    expect(isDrag(DRAG_THRESHOLD, 0)).toBe(true);
    expect(isDrag(-5, -5)).toBe(true);
  });

  it('the window can be resized from every edge and corner', () => {
    expect(new Set(EDGES).size).toBe(8);
  });
});
