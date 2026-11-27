import { describe, expect, it } from 'vitest';
import { aimAt, pickGaze, STATE_GAZE } from './gaze';

describe('where she looks', () => {
  it('follows the order of the design', () => {
    expect(pickGaze({ reaction: true, state: true, emotion: true })).toBe('reaction');
    expect(pickGaze({ state: true, emotion: true })).toBe('state');
    expect(pickGaze({ emotion: true })).toBe('emotion');
    expect(pickGaze({})).toBe('wander');
  });

  it('has a gaze for every state of the body', () => {
    for (const s of ['thinking', 'working', 'confirm', 'listening'] as const) expect(STATE_GAZE[s]).toHaveLength(2);
  });
});

describe('turning to a gaze', () => {
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
