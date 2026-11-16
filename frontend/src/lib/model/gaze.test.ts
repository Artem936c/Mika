import { describe, expect, it } from 'vitest';
import { CURSOR_INTEREST_MS, aimAt, angleTo, cursorInterest, pickGaze, STATE_GAZE } from './gaze';

describe('where she looks', () => {
  it('follows the order of the design', () => {
    expect(
      pickGaze({
        reaction: true,
        typing: true,
        state: true,
        cursor: true,
        emotion: true,
      }),
    ).toBe('reaction');
    expect(pickGaze({ typing: true, state: true, cursor: true, emotion: true })).toBe('typing');
    expect(pickGaze({ state: true, cursor: true, emotion: true })).toBe('state');
    expect(pickGaze({ cursor: true, emotion: true })).toBe('cursor');
    expect(pickGaze({ emotion: true })).toBe('emotion');
    expect(pickGaze({})).toBe('wander');
  });

  it('has a gaze for every state of the body', () => {
    for (const s of ['thinking', 'working', 'confirm', 'listening'] as const) expect(STATE_GAZE[s]).toHaveLength(2);
  });
});

describe('turning to a point', () => {
  it('looks to her left for a point on the right of the screen, and up for a point above', () => {
    const a = angleTo({ x: 300, y: 50 }, { x: 100, y: 100 }, 400);
    expect(a.yaw).toBeGreaterThan(0);
    expect(a.pitch).toBeGreaterThan(0);
    expect(angleTo({ x: 0, y: 100 }, { x: 100, y: 100 }, 400).yaw).toBeLessThan(0);
  });

  it('turns the head at most 35 degrees aside and 20 up or down; the eyes do the rest', () => {
    const far = aimAt(60, -40);
    expect(far.headYaw).toBe(35);
    expect(far.headPitch).toBe(-20);
    expect(far.eyeYaw).toBeGreaterThan(0);
    expect(far.eyePitch).toBeLessThan(0);
    expect(Math.abs(far.eyeYaw)).toBeLessThanOrEqual(15);
  });

  it('turns the body too past 25 degrees', () => {
    expect(aimAt(20, 0).bodyYaw).toBe(0);
    expect(aimAt(45, 0).bodyYaw).toBeGreaterThan(0);
    expect(aimAt(-45, 0).bodyYaw).toBeLessThan(0);
  });

  it('a near point moves the head only, not the eyes', () => {
    const near = aimAt(10, 5);
    expect(near).toEqual({
      headYaw: 10,
      headPitch: 5,
      eyeYaw: 0,
      eyePitch: 0,
      bodyYaw: 0,
    });
  });
});

describe('interest in the cursor', () => {
  it('goes after 5 seconds of a still cursor', () => {
    expect(CURSOR_INTEREST_MS).toBe(5000);
    expect(cursorInterest(1000, 3000)).toBe(true);
    expect(cursorInterest(1000, 6001)).toBe(false);
  });
});
