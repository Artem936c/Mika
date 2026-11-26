import { describe, expect, it } from 'vitest';
import { EMOTION_ORDER } from '../avatar';
import { HEAD_LIMITS, IDLE_ACTIONS, type IdleAction } from './body';
import { MANNERS, MANNER_IDS, mannerOf, reactionFor, restingEmotion } from './manner';
import { REST_POSE_NAMES } from './poses';
import type { TouchPart } from './touch';

const PARTS: TouchPart[] = ['crown', 'face', 'hand', 'other'];

describe('the manner of her character', () => {
  it('exists for every ready character and for an own one', () => {
    expect(MANNER_IDS).toEqual(['caring', 'cheerful', 'calm', 'bold', 'business', 'sarcastic', 'sleepy', 'custom']);
    for (const id of MANNER_IDS) expect(MANNERS[id], id).toBeDefined();
  });

  it('is the caring one for an unknown or missing character', () => {
    expect(mannerOf('pirate')).toBe(MANNERS.caring);
    expect(mannerOf(undefined)).toBe(MANNERS.caring);
    expect(mannerOf('custom')).toBe(MANNERS.custom);
  });

  it('keeps her head, sway and hands within reason', () => {
    for (const id of MANNER_IDS) {
      const m = MANNERS[id];
      expect(Math.abs(m.carriage[0]), id).toBeLessThanOrEqual(HEAD_LIMITS.down / 2);
      expect(Math.abs(m.carriage[1]), id).toBeLessThanOrEqual(8);
      expect(Math.abs(m.carriage[2]), id).toBeLessThanOrEqual(HEAD_LIMITS.roll / 2);
      expect(m.sway, id).toBeGreaterThan(0);
      expect(m.sway, id).toBeLessThanOrEqual(1.5);
      expect(m.hands, id).toBeGreaterThan(0);
      expect(m.hands, id).toBeLessThanOrEqual(1.5);
      expect(m.actionPause, id).toBeGreaterThan(0);
    }
  });

  it('makes the characters differ in body, not only in words', () => {
    const looks = new Set(MANNER_IDS.filter((id) => id !== 'custom').map((id) => JSON.stringify(MANNERS[id])));
    expect(looks.size).toBe(7);
    expect(MANNERS.cheerful.sway).toBeGreaterThan(MANNERS.business.sway);
    expect(MANNERS.cheerful.hands).toBeGreaterThan(MANNERS.sleepy.hands);
    // Bold holds her chin up, sleepy lets her head hang.
    expect(MANNERS.bold.carriage[0]).toBeLessThan(0);
    expect(MANNERS.sleepy.carriage[0]).toBeGreaterThan(0);
  });

  it('picks idle actions and auto poses she has', () => {
    for (const id of MANNER_IDS) {
      const m = MANNERS[id];
      expect(m.actions.length, id).toBeGreaterThan(0);
      for (const a of m.actions) expect(Object.keys(IDLE_ACTIONS), id).toContain(a as IdleAction);
      expect(m.autoPoses.length, id).toBeGreaterThan(0);
      for (const p of m.autoPoses) expect(REST_POSE_NAMES, id).toContain(p);
    }
  });
});

describe('her face at rest', () => {
  it('is sleepy for the sleepy character only while nothing goes on', () => {
    expect(restingEmotion('neutral', 'idle', MANNERS.sleepy)).toBe('sleepy');
    expect(restingEmotion('neutral', 'thinking', MANNERS.sleepy)).toBe('neutral');
    expect(restingEmotion('joy', 'idle', MANNERS.sleepy)).toBe('joy');
  });

  it('stays as the emotion core says for the others', () => {
    for (const id of MANNER_IDS.filter((i) => i !== 'sleepy')) {
      expect(restingEmotion('neutral', 'idle', MANNERS[id]), id).toBe('neutral');
    }
  });
});

describe('her body after a click', () => {
  it('answers every part with every face she can make', () => {
    for (const part of PARTS) {
      for (const emotion of EMOTION_ORDER) {
        const tl = reactionFor(part, emotion);
        expect(tl, `${part} ${emotion}`).not.toBeNull();
        expect(tl!.ms, `${part} ${emotion}`).toBeGreaterThanOrEqual(800);
        expect(tl!.keys.length, `${part} ${emotion}`).toBeGreaterThan(0);
      }
    }
  });

  it('turns away with her arms crossed when cross, wherever she was clicked', () => {
    for (const part of PARTS) {
      const key = reactionFor(part, 'angry')!.keys[0];
      expect(key.leftUpperArm, part).toBeDefined();
      expect(key.rightUpperArm, part).toBeDefined();
      expect(Math.abs(key.head![1]), part).toBeGreaterThanOrEqual(10);
    }
  });

  it('leans back when a click on her body startles her', () => {
    const key = reactionFor('other', 'surprise')!.keys[0];
    expect(key.spine![0]).toBeLessThan(0);
  });

  it('waves back at a hand, leans into a pat and looks away when shy of a touch to the face', () => {
    const wave = reactionFor('hand', 'joy')!;
    expect(new Set(wave.keys.map((k) => JSON.stringify(k.rightHand))).size).toBeGreaterThan(1);
    expect(reactionFor('crown', 'joy')!.keys[0].head![2]).not.toBe(0);
    expect(Math.abs(reactionFor('face', 'shy')!.keys[0].head![1])).toBeGreaterThanOrEqual(15);
  });

  it('keeps every turn of the head within the limits', () => {
    for (const part of PARTS) {
      for (const emotion of EMOTION_ORDER) {
        for (const key of reactionFor(part, emotion)!.keys) {
          const head = key.head ?? [0, 0, 0];
          expect(head[0]).toBeLessThanOrEqual(HEAD_LIMITS.down);
          expect(head[0]).toBeGreaterThanOrEqual(-HEAD_LIMITS.up);
          expect(Math.abs(head[1])).toBeLessThanOrEqual(HEAD_LIMITS.yaw);
          expect(Math.abs(head[2])).toBeLessThanOrEqual(HEAD_LIMITS.roll);
        }
      }
    }
  });
});
