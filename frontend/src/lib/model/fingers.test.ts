import { describe, expect, it } from 'vitest';
import { fingerAxes, fingerTurn, palmOf, rotate, type Vec } from './fingers';

// A hand in the T-pose of VRM 1.0: the left arm along +X, the palm down,
// the thumb towards +Z (forward), the little finger towards -Z.
const left = {
  hand: [0, 0, 0] as Vec,
  index: [0.08, 0, 0.02] as Vec,
  little: [0.07, 0, -0.03] as Vec,
};
// The right hand is the mirror image across X = 0.
const flip = (v: Vec): Vec => [-v[0], v[1], v[2]];
const right = { hand: flip(left.hand), index: flip(left.index), little: flip(left.little) };

const tip = (side: 'left' | 'right', joint: Vec, next: Vec, value: [number, number, number]) => {
  const h = side === 'left' ? left : right;
  const axes = fingerAxes(joint, next, palmOf(h.hand, h.index, h.little, [0, -1, 0]));
  const q = fingerTurn(axes, value, side);
  const d: Vec = [next[0] - joint[0], next[1] - joint[1], next[2] - joint[2]];
  const len = Math.hypot(...d);
  return rotate(q, [d[0] / len, d[1] / len, d[2] / len]);
};

describe('fingers bend about their own axes', () => {
  it('a negative Z curls a finger of the left hand down to the palm', () => {
    const t = tip('left', left.index, [0.11, 0, 0.02], [0, 0, -90]);
    expect(t[1]).toBeLessThan(-0.9);
  });

  it('the same value through the mirror curls the right hand down too', () => {
    const t = tip('right', right.index, flip([0.11, 0, 0.02]), [0, 0, 90]);
    expect(t[1]).toBeLessThan(-0.9);
  });

  it('a finger that points aside in the T-pose still curls to the palm, not sideways', () => {
    // Fanned out by 25 degrees towards the thumb.
    const a = (25 * Math.PI) / 180;
    const next: Vec = [0.08 + 0.03 * Math.cos(a), 0, 0.02 + 0.03 * Math.sin(a)];
    const t = tip('left', left.index, next, [0, 0, -90]);
    expect(t[1]).toBeLessThan(-0.85);
  });

  it('a positive Y turns a finger towards the little finger on both hands', () => {
    expect(tip('left', left.index, [0.11, 0, 0.02], [0, 20, 0])[2]).toBeLessThan(-0.2);
    expect(tip('right', right.index, flip([0.11, 0, 0.02]), [0, -20, 0])[2]).toBeLessThan(-0.2);
  });

  it('the thumb curls down and across the palm', () => {
    const joint: Vec = [0.02, -0.01, 0.03];
    const next: Vec = [0.045, -0.01, 0.055];
    const t = tip('left', joint, next, [0, 0, -60]);
    expect(t[1]).toBeLessThan(-0.3);
    const d = [0.025, 0, 0.025].map((v) => v / Math.hypot(0.025, 0.025));
    // Less forward than before: it goes across, towards the little finger.
    expect(t[2]).toBeLessThan(d[2]);
  });

  it('no turn leaves the finger as it is', () => {
    const t = tip('left', left.index, [0.11, 0, 0.02], [0, 0, 0]);
    expect(t[0]).toBeCloseTo(1, 6);
  });

  it('a finger with no length still gets finite axes', () => {
    const axes = fingerAxes(left.index, left.index, palmOf(left.hand, left.index, left.little, [0, -1, 0]));
    for (const v of [axes.flex, axes.spread, axes.twist]) for (const c of v) expect(Number.isFinite(c)).toBe(true);
  });
});
