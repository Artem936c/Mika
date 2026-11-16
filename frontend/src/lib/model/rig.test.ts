import { describe, expect, it } from 'vitest';
import { add, mirror, mix, withSign, type RigPose } from './rig';

describe('rig', () => {
  const left: RigPose = {
    leftUpperArm: [-8, 10, -72],
    leftIndexProximal: [0, 0, -12],
  };

  it('mirrors the left side to the right and back', () => {
    const right = mirror(left);
    expect(right.rightUpperArm).toEqual([-8, -10, 72]);
    expect(right.rightIndexProximal).toEqual([0, 0, 12]);
    expect(mirror(right)).toEqual({
      leftUpperArm: [-8, 10, -72],
      leftIndexProximal: [0, 0, -12],
    });
  });

  it('mirrors a bone of the middle in place', () => {
    expect(mirror({ head: [5, 10, 3] })).toEqual({ head: [5, -10, -3] });
  });

  it('turns X and Z the other way on a VRM 0.x rig and leaves Y', () => {
    expect(withSign({ head: [5, 10, 3] }, -1)).toEqual({ head: [-5, 10, -3] });
    expect(withSign({ head: [5, 10, 3] }, 1)).toEqual({ head: [5, 10, 3] });
  });

  it('mixes towards the bones of the second pose only', () => {
    const a: RigPose = { head: [0, 0, 0], neck: [10, 0, 0] };
    const b: RigPose = { head: [20, 10, 0] };
    expect(mix(a, b, 0)).toEqual(a);
    expect(mix(a, b, 1)).toEqual({ head: [20, 10, 0], neck: [10, 0, 0] });
    expect(mix(a, b, 0.5).head).toEqual([10, 5, 0]);
    // A bone the first pose lacks comes from rest.
    expect(mix({}, b, 0.5).head).toEqual([10, 5, 0]);
  });

  it('adds offsets with a weight', () => {
    expect(add({ head: [1, 2, 3] }, { head: [10, 10, 10], spine: [4, 0, 0] }, 0.5)).toEqual({
      head: [6, 7, 8],
      spine: [2, 0, 0],
    });
  });
});
