// How she stands: five rest poses of the whole arm down to the last finger
// (decision 0040). Our own angles, written for a VRM 1.0 rig, left side; the
// right side is the mirror image. Pure data, no DOM.

import { both, type RigPose } from './rig';

export const REST_POSE_NAMES = ['relaxed', 'clasped', 'behind', 'hip', 'crossed'] as const;
export type RestPose = (typeof REST_POSE_NAMES)[number];

/** Fingers at ease: a little bent, the little finger the most. */
export const FINGERS_SOFT: RigPose = {
  leftThumbMetacarpal: [0, -6, 4],
  leftThumbProximal: [0, -10, -4],
  leftThumbDistal: [0, -12, -6],
  leftIndexProximal: [0, 2, -10],
  leftIndexIntermediate: [0, 0, -14],
  leftIndexDistal: [0, 0, -10],
  leftMiddleProximal: [0, 0, -13],
  leftMiddleIntermediate: [0, 0, -17],
  leftMiddleDistal: [0, 0, -12],
  leftRingProximal: [0, -2, -16],
  leftRingIntermediate: [0, 0, -20],
  leftRingDistal: [0, 0, -13],
  leftLittleProximal: [0, -5, -19],
  leftLittleIntermediate: [0, 0, -23],
  leftLittleDistal: [0, 0, -15],
};

/** Fingers closed round something: the other hand, an arm. */
export const FINGERS_CURL: RigPose = {
  leftThumbMetacarpal: [0, -14, 6],
  leftThumbProximal: [0, -18, -10],
  leftThumbDistal: [0, -20, -12],
  leftIndexProximal: [0, 0, -42],
  leftIndexIntermediate: [0, 0, -50],
  leftIndexDistal: [0, 0, -30],
  leftMiddleProximal: [0, 0, -46],
  leftMiddleIntermediate: [0, 0, -54],
  leftMiddleDistal: [0, 0, -32],
  leftRingProximal: [0, 0, -48],
  leftRingIntermediate: [0, 0, -56],
  leftRingDistal: [0, 0, -34],
  leftLittleProximal: [0, 0, -50],
  leftLittleIntermediate: [0, 0, -58],
  leftLittleDistal: [0, 0, -36],
};

const relaxed = both({
  leftShoulder: [0, 0, -4],
  leftUpperArm: [-5, 0, -74],
  leftLowerArm: [0, -18, 0],
  leftHand: [0, 0, -6],
  ...FINGERS_SOFT,
});

const clasped = both({
  leftShoulder: [0, 0, -3],
  leftUpperArm: [-10, -45, -84],
  leftLowerArm: [0, -35, 0],
  leftHand: [10, 0, -8],
  ...FINGERS_CURL,
});

const behind = both({
  leftShoulder: [0, 0, -2],
  leftUpperArm: [45, -40, -78],
  leftLowerArm: [0, -95, 0],
  leftHand: [0, 0, -10],
  ...FINGERS_SOFT,
});

/** Elbows by her sides, forearms across under the chest, each hand tucked at the other elbow. */
const crossed = both({
  leftShoulder: [0, 6, -2],
  leftUpperArm: [0, -75, -68],
  leftLowerArm: [0, -90, 0],
  leftHand: [0, -40, 0],
  ...FINGERS_CURL,
});

/** Her left hand rests on the side of her waist, the elbow out and back; the right arm hangs at ease. */
const hip: RigPose = {
  ...relaxed,
  leftShoulder: [0, 0, 2],
  leftUpperArm: [50, -34, -35],
  leftLowerArm: [0, -80, 0],
  leftHand: [0, 10, 10],
};

export const REST_POSES: Record<RestPose, RigPose> = {
  relaxed,
  clasped,
  behind,
  hip,
  crossed,
};

/** The rest pose of a name from the settings; anything else is the relaxed one. */
export function restPoseOf(name: string): RigPose {
  return (REST_POSES as Record<string, RigPose>)[name] ?? REST_POSES.relaxed;
}
