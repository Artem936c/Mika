import { describe, expect, it } from 'vitest';
import { EMOTION_ORDER, type Emotion } from '../avatar';
import {
  FPS,
  VOWELS,
  approach,
  glide,
  blinkWeight,
  breath,
  canModelBlink,
  frameDue,
  frameFor,
  gazeWander,
  gestureAt,
  lipGain,
  nextVowel,
  rigSign,
  shouldRender,
  slotPose,
  slotWeights,
  trunkPose,
  type Profile,
  type Slot,
  type Vowel,
} from './layers';
import { withSign } from './rig';

const slot = (mix: [string, number][], gaze?: [number, number]): Slot => ({
  source: 'auto',
  mix,
  gaze,
});

function profile(lip: number, mix: [string, number][] = []): Profile {
  const slots = Object.fromEntries(EMOTION_ORDER.map((e) => [e, slot(mix)])) as Record<Emotion, Slot>;
  return {
    version: 1,
    format: 'vrm',
    spec: '1.0',
    title: 'T',
    author: 'A',
    license: '',
    expressions: [],
    slots,
    lip,
  };
}

describe('blinking', () => {
  it('a blink goes up, holds and comes down', () => {
    expect(blinkWeight(-5)).toBe(0);
    expect(blinkWeight(0)).toBe(0);
    expect(blinkWeight(25)).toBeCloseTo(0.5);
    expect(blinkWeight(50)).toBe(1);
    expect(blinkWeight(140)).toBe(1);
    expect(blinkWeight(165)).toBeCloseTo(0.5);
    expect(blinkWeight(190)).toBe(0);
    expect(blinkWeight(500)).toBe(0);
  });

  it('a slot that closes the eyes does not blink', () => {
    expect(canModelBlink(slot([['blink', 0.6]]))).toBe(false);
    expect(canModelBlink(slot([['blinkLeft', 1]]))).toBe(false);
    expect(canModelBlink(slot([['blinkRight', 0.5]]))).toBe(false);
    expect(canModelBlink(slot([['blink', 0.2]]))).toBe(true);
    expect(canModelBlink(slot([['happy', 1]]))).toBe(true);
    expect(canModelBlink(slot([]))).toBe(true);
  });
});

describe('lips', () => {
  it('the mouth never repeats a shape', () => {
    const seen = new Set<Vowel | null>();
    let prev: Vowel | null = null;
    for (let i = 0; i < 200; i++) {
      const next = nextVowel(prev, (i * 0.618034) % 1);
      expect(next).not.toBe(prev);
      seen.add(next);
      prev = next;
    }
    expect(seen.has(null)).toBe(true);
    for (const v of VOWELS) expect(seen.has(v)).toBe(true);
    // The ends of the random range are inside the list.
    expect(nextVowel(null, 0)).not.toBeUndefined();
    expect(nextVowel(null, 0.999999)).not.toBeUndefined();
  });

  it('lips are quieter under an emotion', () => {
    expect(lipGain(profile(1), 'neutral')).toBe(1);
    expect(lipGain(profile(1), 'joy')).toBeCloseTo(0.6);
    expect(lipGain(profile(1.5), 'neutral')).toBe(1.5);
    expect(lipGain(profile(0), 'sad')).toBe(0);
  });
});

describe('smoothing', () => {
  it('reaches the target and never overshoots', () => {
    expect(approach(0, 1, 60, 60)).toBeCloseTo(0.632, 3);
    expect(approach(0, 1, 1e6, 60)).toBe(1);
    expect(approach(1, 1, 16, 60)).toBe(1);
    expect(approach(1, 0, 60, 60)).toBeCloseTo(0.368, 3);
    // No time, no move; a time constant of zero jumps.
    expect(approach(0.3, 1, 0, 60)).toBeCloseTo(0.3);
    expect(approach(0.3, 1, 16, 0)).toBe(1);
  });
});

describe('emotion', () => {
  it('weights skip expressions the model lacks', () => {
    const weights = slotWeights(
      slot([
        ['happy', 1],
        ['nope', 0.5],
      ]),
      new Set(['happy']),
    );
    expect([...weights]).toEqual([['happy', 1]]);
  });

  it('a weight from the profile stays between 0 and 1', () => {
    const weights = slotWeights(
      slot([
        ['a', 7],
        ['b', -2],
        ['c', Number.NaN],
      ]),
      new Set(['a', 'b', 'c']),
    );
    expect([...weights]).toEqual([
      ['a', 1],
      ['b', 0],
      ['c', 0],
    ]);
  });

  it('a model without expressions gives empty weights', () => {
    const p = profile(1);
    for (const e of EMOTION_ORDER) {
      expect(slotWeights(p.slots[e], new Set()).size).toBe(0);
      const pose = slotPose(e);
      for (const v of Object.values(pose)) expect(Number.isFinite(v)).toBe(true);
    }
  });

  it('the neutral slot has no pose and no gesture', () => {
    expect(slotPose('neutral')).toEqual({
      pitch: 0,
      yaw: 0,
      roll: 0,
      spine: 0,
    });
    expect(gestureAt('neutral', 0)).toBeNull();
    expect(slotPose('sad').pitch).toBeGreaterThan(0);
    expect(slotPose('think')).toEqual({
      pitch: -6,
      yaw: -10,
      roll: 6,
      spine: 0,
    });
  });
});

describe('gestures', () => {
  it('a poke is a hop that ends', () => {
    expect(gestureAt('poke', 0)).toEqual({ y: 0, roll: 0 });
    expect(gestureAt('poke', 225)!.y).toBeCloseTo(0.03);
    expect(gestureAt('poke', 450)!.y).toBeCloseTo(0);
    expect(gestureAt('poke', 451)).toBeNull();
    expect(gestureAt('poke', -1)).toBeNull();
  });

  it('every emotion but the calm one begins with a movement that ends', () => {
    for (const e of EMOTION_ORDER) {
      if (e === 'neutral') continue;
      const first = gestureAt(e, 0);
      expect(first, e).toEqual({ y: 0, roll: 0 });
      let moved = false;
      for (let ms = 0; ms <= 2400; ms += 25) {
        const g = gestureAt(e, ms);
        if (g && (Math.abs(g.y) > 0.001 || Math.abs(g.roll) > 0.1)) moved = true;
      }
      expect(moved, e).toBe(true);
      expect(gestureAt(e, 2401), e).toBeNull();
    }
  });

  it('joy jumps up', () => {
    expect(gestureAt('joy', 175)!.y).toBeCloseTo(0.035);
  });

  it('the eyes wander a little and come back', () => {
    for (const r of [0, 0.25, 0.5, 0.999]) {
      const g = gazeWander(r, 1 - r);
      expect(Math.abs(g.yaw)).toBeLessThanOrEqual(10);
      expect(Math.abs(g.pitch)).toBeLessThanOrEqual(4);
      expect(g.ms).toBeGreaterThanOrEqual(1200);
      expect(g.ms).toBeLessThanOrEqual(2200);
    }
    expect(gazeWander(0, 0.5).yaw).toBeCloseTo(-10);
    expect(gazeWander(0.999999, 0.5).yaw).toBeCloseTo(10, 3);
  });

  it('breathing is slow and small', () => {
    expect(breath(0)).toBeCloseTo(0);
    expect(breath(1000)).toBeCloseTo(1.5);
    expect(breath(3000)).toBeCloseTo(-1.5);
    expect(breath(4000)).toBeCloseTo(0);
  });
});

describe('rig', () => {
  const pose = { pitch: 12, yaw: 6, roll: 4, spine: 3 };

  it('a VRM 0.x model faces the other way, so its side and forward axes are turned', () => {
    expect(rigSign('0')).toBe(-1);
    expect(rigSign('1')).toBe(1);
    expect(rigSign(undefined)).toBe(1);
  });

  it('the trunk of a VRM 1.0 model: head down is a positive turn about X', () => {
    expect(trunkPose(pose, 1.5, 2, true)).toEqual({
      head: [12, 6, -4],
      spine: [3, 0, 0],
      chest: [1.5, 0, 0],
      hips: [0, 0, -2],
    });
  });

  it('a model without a chest breathes with its spine', () => {
    expect(trunkPose(pose, 1.5, 0, false)).toEqual({
      head: [12, 6, -4],
      spine: [4.5, 0, 0],
      hips: [0, 0, 0],
    });
  });

  it('a VRM 0.x model gets the same trunk with X and Z turned and Y kept', () => {
    expect(withSign(trunkPose(pose, 1.5, 2, true), -1)).toEqual({
      head: [-12, 6, 4],
      spine: [-3, 0, 0],
      chest: [-1.5, 0, 0],
      hips: [0, 0, 2],
    });
  });

  it('at rest the trunk is not turned', () => {
    const rest = trunkPose({ pitch: 0, yaw: 0, roll: 0, spine: 0 }, 0, 0, true);
    expect(
      Object.values(rest)
        .flat()
        .every((v) => v === 0),
    ).toBe(true);
  });
});

describe('camera', () => {
  it('the full view runs from above the head to the middle of the thigh', () => {
    // Like the built-in pictures: the places that show her cut the top of the frame a little.
    const f = frameFor('full', 1.4, 0.9, 1.55);
    const bottom = 0.9 - 0.3 * (1.55 - 0.9);
    expect(f.height).toBeCloseTo((1.55 - bottom) / 0.88);
    expect(f.centerY - f.height / 2).toBeCloseTo(bottom);
    // The top of the head is 12% of the frame below its top edge.
    expect(f.centerY + f.height / 2 - 1.55).toBeCloseTo(0.12 * f.height);
  });

  it('the head view is a square around the head', () => {
    // The head fills the chip: the frame is 1.7 heads tall, centred a little below the middle of the head.
    const f = frameFor('head', 1.4, 0.9, 1.55);
    expect(f.centerY).toBeCloseTo(1.4675);
    expect(f.height).toBeCloseTo(0.255);
  });

  it('the waist view runs from above the head to a little above the hips', () => {
    const f = frameFor('waist', 1.4, 0.9, 1.55);
    const bottom = f.centerY - f.height / 2;
    expect(bottom).toBeGreaterThan(0.9);
    expect(bottom).toBeLessThan(1.1);
    expect(f.centerY + f.height / 2).toBeGreaterThan(1.55);
    // Closer than the full view.
    expect(f.height).toBeLessThan(frameFor('full', 1.4, 0.9, 1.55).height);
  });

  it('the close view shows the head and the shoulders', () => {
    const f = frameFor('close', 1.4, 0.9, 1.55);
    const bottom = f.centerY - f.height / 2;
    expect(bottom).toBeLessThan(1.4 - 0.1);
    expect(bottom).toBeGreaterThan(0.9);
    expect(f.centerY + f.height / 2).toBeGreaterThan(1.55);
    expect(f.height).toBeLessThan(frameFor('waist', 1.4, 0.9, 1.55).height);
  });

  it('a model with broken bones still gets a frame', () => {
    const f = frameFor('full', 1, 1, 1);
    expect(f.height).toBeGreaterThan(0);
    expect(frameFor('head', 1, 1, 1).height).toBeGreaterThan(0);
  });
});

describe('drawing', () => {
  it('nothing is drawn when the view is not seen', () => {
    expect(
      shouldRender({
        attached: true,
        windowShown: true,
        documentHidden: false,
      }),
    ).toBe(true);
    expect(
      shouldRender({
        attached: false,
        windowShown: true,
        documentHidden: false,
      }),
    ).toBe(false);
    expect(
      shouldRender({
        attached: true,
        windowShown: false,
        documentHidden: false,
      }),
    ).toBe(false);
    expect(shouldRender({ attached: true, windowShown: true, documentHidden: true })).toBe(false);
  });

  it('frames come no faster than 30 a second', () => {
    expect(FPS).toBe(30);
    expect(frameDue(1000, 980)).toBe(false);
    expect(frameDue(1016.7, 1000)).toBe(false);
    expect(frameDue(1034, 1000)).toBe(true);
    // Every second frame of a 60 Hz screen, even when it comes a hair early.
    expect(frameDue(1033.2, 1000)).toBe(true);
  });
});

describe('glide', () => {
  const run = (target: number, ms: number, frame: number, smooth = 250, maxSpeed?: number) => {
    const s = { x: 0, v: 0 };
    const path: number[] = [];
    for (let t = 0; t < ms; t += frame) {
      glide(s, target, frame, smooth, maxSpeed);
      path.push(s.x);
    }
    return { s, path };
  };

  it('starts slowly instead of jumping at a new target', () => {
    const { path } = run(30, 16, 16);
    expect(path[0]).toBeGreaterThan(0);
    expect(path[0]).toBeLessThan(1.5);
  });

  it('speeds up, then slows down, without overshoot', () => {
    const { path } = run(30, 1200, 16);
    const steps = path.map((x, i) => x - (path[i - 1] ?? 0));
    const peak = steps.indexOf(Math.max(...steps));
    expect(peak).toBeGreaterThan(1);
    expect(peak).toBeLessThan(steps.length - 1);
    expect(Math.max(...path)).toBeLessThanOrEqual(30);
    expect(path[path.length - 1]).toBeCloseTo(30, 0);
  });

  it('does not depend on the frame rate', () => {
    const slow = run(30, 320, 32).s.x;
    const fast = run(30, 320, 4).s.x;
    expect(Math.abs(slow - fast)).toBeLessThan(1);
  });

  it('keeps under the speed limit, degrees a second', () => {
    const { path } = run(90, 1000, 16, 250, 120);
    for (let i = 1; i < path.length; i++) expect(path[i] - path[i - 1]).toBeLessThanOrEqual((120 * 16) / 1000 + 1e-9);
  });

  it('does not stop dead when a moving target passes it', () => {
    const s = { x: 0, v: 0.2 };
    glide(s, 1, 16, 250);
    expect(s.v).toBeGreaterThan(0);
  });

  it('stays put on a zero step and lands after a long pause', () => {
    const s = { x: 3, v: 0 };
    glide(s, 10, 0, 250);
    expect(s.x).toBe(3);
    glide(s, 10, 60000, 250);
    expect(s.x).toBeCloseTo(10, 3);
    expect(Number.isFinite(s.v)).toBe(true);
  });
});
