import { describe, expect, it } from 'vitest';
import { EMOTION_ORDER } from '../avatar';
import {
  BODY_STATES,
  BODY_TAU_MS,
  GESTURES,
  GESTURE_NAMES,
  EMOTION_BODY,
  IDLE_ACTIONS,
  STATE_BODY,
  autoPosePause,
  beatOffset,
  beats,
  bodyState,
  endCue,
  idleActionPause,
  idleOffset,
  nextAutoPose,
  autoPoseNow,
  saccade,
  twiceBlink,
  sampleTimeline,
  speechStrength,
  ZERO_TRUNK,
  HEAD_LIMITS,
  limitHead,
  type Timeline,
  windowLean,
} from './body';
import { mix } from './rig';

describe('idle', () => {
  it('shifts the weight slowly and only a little', () => {
    let maxHips = 0;
    let prev = idleOffset(0, 0.3);
    for (let ms = 0; ms <= 20000; ms += 33) {
      const now = idleOffset(ms, 0.3);
      maxHips = Math.max(maxHips, ...now.hips!.map(Math.abs));
      // Smooth: no jump between two frames.
      expect(Math.abs(now.hips![2] - prev.hips![2])).toBeLessThan(0.2);
      prev = now;
    }
    expect(maxHips).toBeLessThanOrEqual(4);
    expect(maxHips).toBeGreaterThan(1);
  });

  it('waits 20 to 60 seconds between actions and 2 to 5 minutes between poses', () => {
    expect(idleActionPause(0)).toBe(20000);
    expect(idleActionPause(0.999)).toBeLessThanOrEqual(60000);
    expect(autoPosePause(0)).toBe(120000);
    expect(autoPosePause(0.999)).toBeLessThanOrEqual(300000);
  });

  it('has the four actions of the design', () => {
    expect(Object.keys(IDLE_ACTIONS).sort()).toEqual(['hair', 'look_around', 'look_hand', 'stretch']);
  });
});

describe('timeline', () => {
  const tl: Timeline = {
    ms: 1000,
    keys: [{ head: [0, 0, 0] }, { head: [10, 0, 0] }, { head: [0, 0, 0] }],
  };

  it('is nothing before it begins and after it ends', () => {
    expect(sampleTimeline(tl, -1)).toBe(null);
    expect(sampleTimeline(tl, 1001)).toBe(null);
  });

  it('passes through its keys and comes in and goes out softly', () => {
    const mid = sampleTimeline(tl, 500)!;
    expect(mid.pose.head![0]).toBeCloseTo(10);
    expect(mid.weight).toBe(1);
    expect(sampleTimeline(tl, 0)!.weight).toBe(0);
    expect(sampleTimeline(tl, 1000)!.weight).toBe(0);
  });
});

describe('body by emotion and state', () => {
  it('has a body for every emotion and every state', () => {
    for (const e of EMOTION_ORDER) expect(EMOTION_BODY[e], e).toBeDefined();
    for (const s of BODY_STATES) expect(STATE_BODY[s], s).toBeDefined();
  });

  it('names the state of the body', () => {
    expect(bodyState('awaiting_confirm', false)).toBe('confirm');
    expect(bodyState('thinking', false)).toBe('thinking');
    expect(bodyState('listening', false)).toBe('listening');
    expect(bodyState('idle', false)).toBe(null);
    expect(bodyState('speaking', false)).toBe(null);
    // A tool at work wins over thinking.
    expect(bodyState('thinking', true)).toBe('working');
  });
});

describe('speech', () => {
  it('finds loud peaks no closer than 600 ms', () => {
    const levels = [0, 200, 0, 0, 220, 0, ...Array(40).fill(0), 255, 0, 100, 0];
    expect(beats(levels, 20)).toEqual([20, 920]);
  });

  it('skips peaks under half of the loudest', () => {
    expect(beats([0, 100, 0, 0, 0], 20)).toEqual([]);
  });

  it('moves a forearm on a beat and comes back', () => {
    const at = beatOffset(150, 'left', 1);
    expect(Math.abs(at.leftLowerArm![1])).toBeGreaterThan(3);
    const after = beatOffset(1000, 'left', 1);
    expect(
      Object.values(after)
        .flat()
        .every((v) => v === 0),
    ).toBe(true);
  });

  it('nods on an exclamation and tilts on a question', () => {
    expect(endCue('Ура!')).toBe('nod');
    expect(endCue('Правда? ')).toBe('tilt');
    expect(endCue('Хорошо.')).toBe(null);
  });

  it('moves less when sad or sleepy and more with joy', () => {
    expect(speechStrength('sad')).toBe(0.5);
    expect(speechStrength('sleepy')).toBe(0.5);
    expect(speechStrength('joy')).toBe(1.3);
    expect(speechStrength('neutral')).toBe(1);
  });
});

describe('the body over time', () => {
  it('changes the auto pose to another one', () => {
    for (const r of [0, 0.3, 0.6, 0.99]) expect(nextAutoPose('relaxed', r)).not.toBe('relaxed');
    expect(nextAutoPose('crossed', 0)).not.toBe('crossed');
  });

  it('changes the auto pose among the poses of her character', () => {
    for (const r of [0, 0.5, 0.99]) expect(nextAutoPose('relaxed', r, ['hip', 'crossed'])).toMatch(/^(hip|crossed)$/);
    expect(nextAutoPose('hip', 0.3, ['hip', 'crossed'])).toBe('crossed');
    // Only one pose to take: she keeps it.
    expect(nextAutoPose('hip', 0.3, ['hip'])).toBe('hip');
  });

  it('leaves a pose her new character does not take at once, and keeps one it does', () => {
    expect(autoPoseNow('relaxed', ['hip', 'crossed'])).toBe(true);
    expect(autoPoseNow('hip', ['hip', 'crossed'])).toBe(false);
  });

  it('keeps every bone of the trunk, so a pose that leaves goes back to rest', () => {
    for (const bone of ['hips', 'spine', 'chest', 'neck', 'head'] as const) expect(ZERO_TRUNK[bone]).toEqual([0, 0, 0]);
    expect(BODY_TAU_MS).toBeGreaterThan(100);
    expect(BODY_TAU_MS).toBeLessThan(200);
  });
});

describe('eyes', () => {
  it('jump a little every 0.6 to 3 seconds', () => {
    for (const [a, b] of [
      [0, 0],
      [0.5, 0.5],
      [0.999, 0.999],
    ]) {
      const s = saccade(a, b);
      expect(Math.abs(s.yaw)).toBeLessThanOrEqual(2);
      expect(Math.abs(s.pitch)).toBeLessThanOrEqual(1);
      expect(s.ms).toBeGreaterThanOrEqual(600);
      expect(s.ms).toBeLessThanOrEqual(3000);
    }
  });

  it('blink twice now and then', () => {
    expect(twiceBlink(0.05)).toBe(true);
    expect(twiceBlink(0.5)).toBe(false);
  });
});

describe('gestures', () => {
  it('are the nine of the design', () => {
    expect(GESTURE_NAMES).toEqual(['wave', 'bow', 'nod', 'shake_head', 'shrug', 'clap', 'peace', 'spin', 'dance']);
  });

  it.each(['wave', 'bow', 'nod', 'shake_head', 'shrug', 'clap', 'peace', 'spin', 'dance'] as const)('%s comes from rest and goes back to it', (name) => {
    const tl = GESTURES[name];
    expect(tl.ms).toBeGreaterThan(300);
    expect(tl.keys.length).toBeGreaterThan(0);
    expect(sampleTimeline(tl, 0)!.weight).toBe(0);
    expect(sampleTimeline(tl, tl.ms)!.weight).toBe(0);
    expect(sampleTimeline(tl, tl.ms / 2)!.weight).toBe(1);
  });
});

describe('a whole turn', () => {
  it('comes in and goes out at once', () => {
    const tl = GESTURES.spin;
    expect(sampleTimeline(tl, tl.ms * 0.1)!.weight).toBe(1);
    expect(sampleTimeline(tl, tl.ms * 0.9)!.weight).toBe(1);
  });
});

describe('a whole turn seen from the front', () => {
  /** Her turn on screen, -180..180: 0 faces the user, ±180 shows her back. */
  const facing = (y: number) => ((((y + 180) % 360) + 360) % 360) - 180;

  it('never shows her back while it comes in or goes out', () => {
    const tl = GESTURES.spin;
    for (const share of [0, 0.005, 0.01, 0.02, 0.03, 0.97, 0.98, 0.99, 0.995, 1]) {
      const s = sampleTimeline(tl, tl.ms * share)!;
      const y = mix({ hips: [0, 0, 0] }, s.pose, s.weight).hips![1];
      expect(Math.abs(facing(y))).toBeLessThan(30);
    }
  });

  it('still turns all the way round in the middle', () => {
    const tl = GESTURES.spin;
    const s = sampleTimeline(tl, tl.ms / 2)!;
    const y = mix({ hips: [0, 0, 0] }, s.pose, s.weight).hips![1];
    expect(Math.abs(facing(y))).toBeGreaterThan(150);
  });
});

describe('a window on the move', () => {
  it('leaves her top behind: moving right, she leans to the left of the screen', () => {
    // A positive turn about Z tips her top to the left of the screen (VRM 1.0, facing the user).
    expect(windowLean(600)).toBeGreaterThan(0);
    expect(windowLean(-600)).toBeLessThan(0);
    expect(windowLean(0)).toBe(0);
  });

  it('never leans more than 8 degrees', () => {
    expect(windowLean(1e6)).toBe(8);
    expect(windowLean(-1e6)).toBe(-8);
  });
});

describe('an arm is moved whole', () => {
  const CHAIN = ['UpperArm', 'LowerArm', 'Hand', 'ThumbProximal', 'IndexProximal', 'MiddleProximal', 'RingProximal', 'LittleProximal'];
  const bodies = [...EMOTION_ORDER.map((e) => [`emotion ${e}`, EMOTION_BODY[e]] as const), ...BODY_STATES.map((s) => [`state ${s}`, STATE_BODY[s]] as const)];

  it.each(bodies)('%s sets the whole arm or leaves the rest pose alone', (_, body) => {
    for (const side of ['left', 'right']) {
      const set = CHAIN.filter((b) => (body as Record<string, unknown>)[side + b] !== undefined);
      expect([0, CHAIN.length], `${side}: ${set.join(', ')}`).toContain(set.length);
    }
  });

  it('joy keeps the arms of the rest pose and only lifts the shoulders', () => {
    expect(EMOTION_BODY.joy.leftUpperArm).toBeUndefined();
    expect(EMOTION_BODY.joy.leftShoulder![2]).toBeGreaterThan(0);
  });

  it('thinking puts her hand under the chin, not at the neck: the elbow is folded more', () => {
    expect(STATE_BODY.thinking.rightLowerArm![1]).toBeGreaterThanOrEqual(135);
    expect(EMOTION_BODY.think.rightLowerArm![1]).toBeGreaterThanOrEqual(135);
  });

  it('listening keeps the elbows down by her sides instead of sticking one out', () => {
    expect(STATE_BODY.listening.leftUpperArm![2]).toBeLessThanOrEqual(-65);
    expect(STATE_BODY.listening.rightUpperArm![2]).toBeGreaterThanOrEqual(65);
  });
});

describe('the head', () => {
  it('turns and tilts no further than a person would, whatever adds up', () => {
    const pose = limitHead({ head: [40, -60, 30], spine: [5, 0, 0] });
    expect(pose.head).toEqual([HEAD_LIMITS.down, -HEAD_LIMITS.yaw, HEAD_LIMITS.roll]);
    expect(pose.spine).toEqual([5, 0, 0]);
    expect(limitHead({ head: [-40, 60, -30] }).head).toEqual([-HEAD_LIMITS.up, HEAD_LIMITS.yaw, -HEAD_LIMITS.roll]);
  });

  it('leaves a head within the limits as it is', () => {
    expect(limitHead({ head: [5, 10, -4] }).head).toEqual([5, 10, -4]);
    expect(limitHead({ spine: [1, 2, 3] })).toEqual({ spine: [1, 2, 3] });
  });

  it('lets her look round and shake her head in full', () => {
    for (const key of [...IDLE_ACTIONS.look_around.keys, ...GESTURES.shake_head.keys]) {
      expect(Math.abs(key.head![1])).toBeLessThanOrEqual(HEAD_LIMITS.yaw);
    }
  });
});
