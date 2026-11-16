import { describe, expect, it } from 'vitest';
import { Probe, REACTION, isOnFigure, nearestBone, partOf, wheelDue } from './touch';

describe('the part of her a click hit', () => {
  it('tells the top of the head from the face', () => {
    expect(partOf('head', 0.7)).toBe('crown');
    expect(partOf('head', 0.3)).toBe('face');
  });

  it('names the arms and hands', () => {
    for (const bone of ['leftUpperArm', 'rightLowerArm', 'leftHand', 'rightIndexProximal', 'leftThumbDistal']) {
      expect(partOf(bone, 0)).toBe('hand');
    }
  });

  it('never tells the chest, hips or legs apart', () => {
    for (const bone of ['chest', 'upperChest', 'spine', 'hips', 'leftUpperLeg', 'rightLowerLeg', 'neck']) {
      expect(partOf(bone, 0)).toBe('other');
    }
  });

  it('picks the nearest bone to the point', () => {
    const bones = {
      head: [0, 1.5, 0],
      leftHand: [0.4, 0.8, 0],
      hips: [0, 0.9, 0],
    } as Record<string, [number, number, number]>;
    expect(nearestBone([0.38, 0.82, 0.02], bones)).toBe('leftHand');
    expect(nearestBone([0, 1.55, 0.05], bones)).toBe('head');
  });

  it('has a movement for each part but the rest', () => {
    expect(REACTION.crown).not.toBe(null);
    expect(REACTION.face).not.toBe(null);
    expect(REACTION.hand).not.toBe(null);
    expect(REACTION.other).toBe(null);
  });
});

describe('the window on the desktop', () => {
  it('takes a click on her and lets the rest through', () => {
    expect(isOnFigure(200)).toBe(true);
    expect(isOnFigure(10)).toBe(false);
  });
});

describe('the probe under the cursor', () => {
  it('reads the latest point once, after the frame is drawn', () => {
    const p = new Probe();
    const seen: number[] = [];
    p.ask(10, 20, (a) => seen.push(a));
    p.ask(30, 40, (a) => seen.push(a + 1000));
    expect(p.take((x, y) => x + y)).toBe(true);
    expect(seen).toEqual([1070]);
    // Nothing more to read until asked again: no read of pixels every frame.
    expect(p.take(() => 5)).toBe(false);
    expect(seen).toEqual([1070]);
  });
});

describe('the wheel', () => {
  it('steps the size at most every 150 ms', () => {
    expect(wheelDue(0, 100)).toBe(false);
    expect(wheelDue(0, 150)).toBe(true);
    expect(wheelDue(-Infinity, 0)).toBe(true);
  });
});
