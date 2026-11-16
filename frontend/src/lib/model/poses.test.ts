import { describe, expect, it } from 'vitest';
import { REST_POSES, REST_POSE_NAMES, restPoseOf } from './poses';
import { mirror, type Euler3, type RigBone } from './rig';

const FINGERS = ['Thumb', 'Index', 'Middle', 'Ring', 'Little'];
const ARM: RigBone[] = ['leftShoulder', 'leftUpperArm', 'leftLowerArm', 'leftHand', 'rightShoulder', 'rightUpperArm', 'rightLowerArm', 'rightHand'];

describe('rest poses', () => {
  it('are the five of the design', () => {
    expect(REST_POSE_NAMES).toEqual(['relaxed', 'clasped', 'behind', 'hip', 'crossed']);
  });

  it.each(REST_POSE_NAMES)('%s sets both arms, both hands and every finger', (name) => {
    const pose = REST_POSES[name];
    for (const bone of ARM) expect(pose[bone], bone).toBeDefined();
    for (const side of ['left', 'right']) {
      for (const f of FINGERS) {
        const bones = Object.keys(pose).filter((b) => b.startsWith(side + f));
        expect(bones.length, side + f).toBe(3);
      }
    }
  });

  it.each(REST_POSE_NAMES.filter((n) => n !== 'hip'))('%s is the same on both sides', (name) => {
    const pose = REST_POSES[name];
    const seen = mirror(pose);
    for (const bone of Object.keys(pose) as RigBone[]) {
      if (!bone.startsWith('left') && !bone.startsWith('right')) continue;
      const a = pose[bone] as Euler3;
      const b = seen[bone] as Euler3;
      a.forEach((v, i) => expect(v, `${bone}[${i}]`).toBeCloseTo(b[i]));
    }
  });

  it.each(REST_POSE_NAMES)('%s keeps every turn within half a circle', (name) => {
    for (const turn of Object.values(REST_POSES[name]) as Euler3[]) {
      for (const v of turn) expect(Math.abs(v)).toBeLessThanOrEqual(180);
    }
  });

  it.each(['relaxed', 'clasped', 'behind'] as const)('%s brings the upper arms down by 60 degrees or more', (name) => {
    const pose = REST_POSES[name];
    expect(pose.leftUpperArm![2]).toBeLessThanOrEqual(-60);
    expect(pose.rightUpperArm![2]).toBeGreaterThanOrEqual(60);
  });

  it.each(['hip', 'crossed'] as const)('%s keeps the upper arms down by 35 degrees or more', (name) => {
    const pose = REST_POSES[name];
    expect(pose.leftUpperArm![2]).toBeLessThanOrEqual(-35);
    expect(pose.rightUpperArm![2]).toBeGreaterThanOrEqual(35);
  });

  it('an unknown name is the relaxed pose', () => {
    expect(restPoseOf('dance')).toBe(REST_POSES.relaxed);
    expect(restPoseOf('crossed')).toBe(REST_POSES.crossed);
  });
});
