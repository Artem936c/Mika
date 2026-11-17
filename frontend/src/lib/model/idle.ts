// The user's idle animation as the rest pose (decision 0041). A frame of the
// clip becomes the angles of a pose, the way the layers write them, so the
// emotion, the gaze and the gestures work over it as over a rest pose. The
// fingers stay with the rest pose: they bend about the axes of the model.
// Pure functions, no DOM.

import { withSign, type Euler3, type RigBone, type RigPose } from './rig';

/** A turn as a quaternion: x, y, z, w. */
export type Quat = [number, number, number, number];

const FINGER = /Thumb|Index|Middle|Ring|Little/;
const deg = (r: number) => (r * 180) / Math.PI + 0;

/** Angles about X, then Y, then Z, degrees, of a turn. */
function eulerOf([x, y, z, w]: Quat): Euler3 {
  const m11 = 1 - 2 * (y * y + z * z);
  const m12 = 2 * (x * y - z * w);
  const m13 = 2 * (x * z + y * w);
  const m22 = 1 - 2 * (x * x + z * z);
  const m23 = 2 * (y * z - x * w);
  const m32 = 2 * (y * z + x * w);
  const m33 = 1 - 2 * (x * x + y * y);
  const ey = Math.asin(Math.max(-1, Math.min(1, m13)));
  if (Math.abs(m13) < 0.9999999) return [deg(Math.atan2(-m23, m33)), deg(ey), deg(Math.atan2(-m12, m11))];
  return [deg(Math.atan2(m32, m22)), deg(ey), 0];
}

/** A frame of the clip as a pose for a rig of `sign` (`rigSign`), without the fingers. */
export function toRig(frame: Iterable<[RigBone, Quat]>, sign: 1 | -1): RigPose {
  const out: RigPose = {};
  for (const [bone, q] of frame) if (!FINGER.test(bone)) out[bone] = eulerOf(q);
  // The clip turns the bones of this model; the layers write angles for VRM 1.0.
  return withSign(out, sign);
}

const shortWay = (by: number) => by - 360 * Math.round(by / 360) + 0;

/** How far each bone of `first` has moved by `now`, the short way. */
export function motionOf(now: RigPose, first: RigPose): RigPose {
  const out: RigPose = {};
  for (const [bone, from] of Object.entries(first) as [RigBone, Euler3][]) {
    const to = now[bone];
    if (to) out[bone] = [shortWay(to[0] - from[0]), shortWay(to[1] - from[1]), shortWay(to[2] - from[2])];
  }
  return out;
}
