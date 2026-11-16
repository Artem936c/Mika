// The skeleton of a model as the layers see it: a turn for each bone of the
// normalized rig, degrees about X, Y and Z in that order (decision 0040).
// Angles are written for a VRM 1.0 model, which faces +Z with its left hand
// at +X; `withSign` turns them for a VRM 0.x one. Pure functions, no DOM.

/** Turn of one bone, degrees: about X, then Y, then Z. */
export type Euler3 = [number, number, number];

/** The bones the layers turn: the trunk, the arms with every finger, the legs. */
export const RIG_BONES = [
  'hips',
  'spine',
  'chest',
  'upperChest',
  'neck',
  'head',
  'leftShoulder',
  'leftUpperArm',
  'leftLowerArm',
  'leftHand',
  'leftThumbMetacarpal',
  'leftThumbProximal',
  'leftThumbDistal',
  'leftIndexProximal',
  'leftIndexIntermediate',
  'leftIndexDistal',
  'leftMiddleProximal',
  'leftMiddleIntermediate',
  'leftMiddleDistal',
  'leftRingProximal',
  'leftRingIntermediate',
  'leftRingDistal',
  'leftLittleProximal',
  'leftLittleIntermediate',
  'leftLittleDistal',
  'rightShoulder',
  'rightUpperArm',
  'rightLowerArm',
  'rightHand',
  'rightThumbMetacarpal',
  'rightThumbProximal',
  'rightThumbDistal',
  'rightIndexProximal',
  'rightIndexIntermediate',
  'rightIndexDistal',
  'rightMiddleProximal',
  'rightMiddleIntermediate',
  'rightMiddleDistal',
  'rightRingProximal',
  'rightRingIntermediate',
  'rightRingDistal',
  'rightLittleProximal',
  'rightLittleIntermediate',
  'rightLittleDistal',
  'leftUpperLeg',
  'leftLowerLeg',
  'leftFoot',
  'rightUpperLeg',
  'rightLowerLeg',
  'rightFoot',
] as const;
export type RigBone = (typeof RIG_BONES)[number];

/** Turns of some bones; a bone left out stays as it was. */
export type RigPose = Partial<Record<RigBone, Euler3>>;

// `+ 0` turns a negative zero into a plain one.
const plain = (v: number) => v + 0;

function otherSide(bone: RigBone): RigBone {
  if (bone.startsWith('left')) return ('right' + bone.slice(4)) as RigBone;
  if (bone.startsWith('right')) return ('left' + bone.slice(5)) as RigBone;
  return bone;
}

/** The same pose seen in a mirror: left becomes right; turns about Y and Z change sign. */
export function mirror(pose: RigPose): RigPose {
  const out: RigPose = {};
  for (const [bone, [x, y, z]] of Object.entries(pose) as [RigBone, Euler3][]) {
    out[otherSide(bone)] = [plain(x), plain(-y), plain(-z)];
  }
  return out;
}

/** A pose for a rig of `sign` (`rigSign`): on VRM 0.x turns about X and Z change sign. */
export function withSign(pose: RigPose, sign: 1 | -1): RigPose {
  if (sign === 1) return pose;
  const out: RigPose = {};
  for (const [bone, [x, y, z]] of Object.entries(pose) as [RigBone, Euler3][]) {
    out[bone] = [plain(-x), plain(y), plain(-z)];
  }
  return out;
}

/** `a` moved by `w` (0..1) towards `b`, for the bones `b` has; a bone `a` lacks starts at rest. */
export function mix(a: RigPose, b: RigPose, w: number): RigPose {
  const out: RigPose = { ...a };
  if (w <= 0) return out;
  for (const [bone, to] of Object.entries(b) as [RigBone, Euler3][]) {
    const from = a[bone] ?? [0, 0, 0];
    out[bone] = [0, 1, 2].map((i) => plain(from[i] + (to[i] - from[i]) * Math.min(1, w))) as Euler3;
  }
  return out;
}

/** `a` with the turns of `b` times `w` added on top. */
export function add(a: RigPose, b: RigPose, w: number): RigPose {
  const out: RigPose = { ...a };
  if (w === 0) return out;
  for (const [bone, by] of Object.entries(b) as [RigBone, Euler3][]) {
    const from = a[bone] ?? [0, 0, 0];
    out[bone] = [0, 1, 2].map((i) => plain(from[i] + by[i] * w)) as Euler3;
  }
  return out;
}

/** Both sides of a pose written for the left side and the middle. */
export function both(leftAndMiddle: RigPose): RigPose {
  const right = mirror(leftAndMiddle);
  const out: RigPose = { ...leftAndMiddle };
  for (const [bone, turn] of Object.entries(right) as [RigBone, Euler3][]) {
    if (bone.startsWith('right')) out[bone] = turn;
  }
  return out;
}
