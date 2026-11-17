// Fingers bend about their own axes (decision 0041). The normalized rig
// lines up the bones, not the fingers: in the T-pose of one model they fan
// out, in another the thumb points forward. So the axes of every finger
// joint come from where the joints of this model stand, and the numbers of
// a pose keep their meaning for the left hand: Z below zero curls towards
// the palm, Y above zero turns towards the little finger, X twists. Pure
// functions, no DOM.

export type Vec = [number, number, number];
/** A turn as a quaternion: x, y, z, w. */
export type Quat = [number, number, number, number];

/** Where a finger joint turns: unit axes in the space they were measured in. */
export interface FingerAxes {
  /** A turn about it by a positive angle curls the finger to the palm. */
  flex: Vec;
  /** A turn about it by a positive angle turns the finger towards the little finger. */
  spread: Vec;
  /** Along the finger. */
  twist: Vec;
}

/** The palm of a hand in the T-pose. */
export interface Palm {
  /** A point under the middle of the palm, one and a half palms down: the fingers curl towards it. */
  target: Vec;
  /** From the index finger to the little finger. */
  towardLittle: Vec;
  /** Down out of the palm. */
  down: Vec;
}

const sub = (a: Vec, b: Vec): Vec => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const addScaled = (a: Vec, b: Vec, k: number): Vec => [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k];
const cross = (a: Vec, b: Vec): Vec => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const length = (a: Vec) => Math.hypot(a[0], a[1], a[2]);

/** `a` of length one; `fallback` when `a` has no length. */
function unit(a: Vec, fallback: Vec): Vec {
  const l = length(a);
  return l > 1e-9 ? [a[0] / l, a[1] / l, a[2] / l] : fallback;
}

/** The palm from the wrist, the base of the index and of the little finger; `down` is down out of the palm in the T-pose. */
export function palmOf(hand: Vec, index: Vec, little: Vec, down: Vec): Palm {
  const base: Vec = [(index[0] + little[0]) / 2, (index[1] + little[1]) / 2, (index[2] + little[2]) / 2];
  const center = addScaled(addScaled([0, 0, 0], hand, 0.3), base, 0.7);
  const size = length(sub(base, hand));
  const d = unit(down, [0, -1, 0]);
  return { target: addScaled(center, d, size * 1.5), towardLittle: unit(sub(little, index), [0, 0, -1]), down: d };
}

/** The axes of a finger joint at `joint`, whose finger goes on to `next`. */
export function fingerAxes(joint: Vec, next: Vec, palm: Palm): FingerAxes {
  const twist = unit(sub(next, joint), [1, 0, 0]);
  const flex = unit(cross(twist, unit(sub(palm.target, joint), palm.down)), unit(cross(twist, palm.down), [0, 0, 1]));
  const spread = unit(cross(twist, palm.towardLittle), [0, 1, 0]);
  return { flex, spread, twist };
}

function aroundAxis(axis: Vec, degrees: number): Quat {
  const half = (degrees * Math.PI) / 360;
  const s = Math.sin(half);
  return [axis[0] * s, axis[1] * s, axis[2] * s, Math.cos(half)];
}

function times(a: Quat, b: Quat): Quat {
  const [ax, ay, az, aw] = a;
  const [bx, by, bz, bw] = b;
  return [
    aw * bx + ax * bw + ay * bz - az * by,
    aw * by - ax * bz + ay * bw + az * bx,
    aw * bz + ax * by - ay * bx + az * bw,
    aw * bw - ax * bx - ay * by - az * bz,
  ];
}

/** `v` turned by `q`. */
export function rotate(q: Quat, v: Vec): Vec {
  const [x, y, z, w] = q;
  const u: Vec = [x, y, z];
  const t = cross(u, v).map((c) => c * 2) as Vec;
  const ut = cross(u, t);
  return [v[0] + w * t[0] + ut[0], v[1] + w * t[1] + ut[1], v[2] + w * t[2] + ut[2]];
}

/**
 * The turn of a finger joint for the numbers of a pose, as stored for that
 * side: the right side holds the mirror image of the left (`mirror`), so Y
 * and Z come back with their sign changed. Twist turns about the finger,
 * which points the other way on the right.
 */
export function fingerTurn(axes: FingerAxes, value: readonly [number, number, number], side: 'left' | 'right'): Quat {
  const [x, y, z] = side === 'left' ? value : [-value[0], -value[1], -value[2]];
  return times(times(aroundAxis(axes.spread, y), aroundAxis(axes.flex, -z)), aroundAxis(axes.twist, x));
}
