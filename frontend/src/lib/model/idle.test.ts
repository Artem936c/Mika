import { describe, expect, it } from 'vitest';
import { motionOf, toRig, type Quat } from './idle';

const aboutZ = (deg: number): Quat => {
  const h = (deg * Math.PI) / 360;
  return [0, 0, Math.sin(h), Math.cos(h)];
};
const aboutX = (deg: number): Quat => {
  const h = (deg * Math.PI) / 360;
  return [Math.sin(h), 0, 0, Math.cos(h)];
};

describe('an idle animation as a pose', () => {
  it('turns a frame of the clip into the angles of a pose', () => {
    const pose = toRig(
      [
        ['leftUpperArm', aboutZ(-70)],
        ['spine', [0, 0, 0, 1]],
      ],
      1,
    );
    expect(pose.leftUpperArm![2]).toBeCloseTo(-70, 4);
    expect(pose.spine).toEqual([0, 0, 0]);
  });

  it('reads a VRM 0.x clip in the angles of a VRM 1.0 pose', () => {
    const pose = toRig(
      [
        ['leftUpperArm', aboutZ(-70)],
        ['head', aboutX(10)],
      ],
      -1,
    );
    expect(pose.leftUpperArm![2]).toBeCloseTo(70, 4);
    expect(pose.head![0]).toBeCloseTo(-10, 4);
  });

  it('leaves the fingers to the rest pose', () => {
    const pose = toRig(
      [
        ['leftIndexProximal', aboutZ(-40)],
        ['rightThumbProximal', aboutZ(20)],
        ['leftHand', aboutZ(-10)],
      ],
      1,
    );
    expect(pose.leftIndexProximal).toBeUndefined();
    expect(pose.rightThumbProximal).toBeUndefined();
    expect(pose.leftHand![2]).toBeCloseTo(-10, 4);
  });

  it('gives the motion since the first frame the short way', () => {
    const m = motionOf({ spine: [2, 0, 179], head: [0, 5, 0] }, { spine: [0, 0, -179], head: [0, 5, 0], chest: [1, 1, 1] });
    expect(m.spine![0]).toBeCloseTo(2);
    expect(m.spine![2]).toBeCloseTo(-2);
    expect(m.head).toEqual([0, 0, 0]);
    expect(m.chest).toBeUndefined();
  });
});
