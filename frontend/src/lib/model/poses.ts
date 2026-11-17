// How she stands: five rest poses of the whole arm down to the last finger
// (decision 0040). Our own angles, written for a VRM 1.0 rig, left side; the
// right side is the mirror image. Pure data, no DOM.

import { both, type RigPose } from './rig';

export const REST_POSE_NAMES = ['relaxed', 'clasped', 'behind', 'hip', 'crossed'] as const;
export type RestPose = (typeof REST_POSE_NAMES)[number];

/**
 * Fingers at ease: a little bent, the little finger the most; the thumb lies
 * along the index finger. Finger angles keep one meaning on every model
 * (decision 0041): Z below zero curls to the palm, Y above zero turns towards
 * the little finger.
 */
export const FINGERS_SOFT: RigPose = {
  leftThumbMetacarpal: [0, 25, -10],
  leftThumbProximal: [0, 10, -15],
  leftThumbDistal: [0, 0, -15],
  leftIndexProximal: [0, 2, -12],
  leftIndexIntermediate: [0, 0, -18],
  leftIndexDistal: [0, 0, -12],
  leftMiddleProximal: [0, 0, -16],
  leftMiddleIntermediate: [0, 0, -22],
  leftMiddleDistal: [0, 0, -14],
  leftRingProximal: [0, -2, -20],
  leftRingIntermediate: [0, 0, -26],
  leftRingDistal: [0, 0, -16],
  leftLittleProximal: [0, -4, -24],
  leftLittleIntermediate: [0, 0, -30],
  leftLittleDistal: [0, 0, -18],
};

/** Fingers closed round something: the other hand, an arm. */
export const FINGERS_CURL: RigPose = {
  leftThumbMetacarpal: [0, 30, -20],
  leftThumbProximal: [0, 10, -25],
  leftThumbDistal: [0, 0, -25],
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
