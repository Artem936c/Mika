// Makes `sample.vrm` beside this file: a figure of boxes with a VRM 1.0
// skeleton, a face that moves and a custom expression. It is our own and
// carries nobody's work; it exists so that the preview (`?mock=...&vrm`) and a
// first import can be tried without a model from somewhere else.
//
//   node frontend/src/dev/make-sample-vrm.mjs      VRM 1.0
//   node frontend/src/dev/make-sample-vrm.mjs 0    VRM 0.x: she faces the other way in her own space
//
// The output is not committed (see .gitignore).

import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** VRM 0.x: the same figure turned half round about the vertical axis, as that format has it. */
const V0 = process.argv[2] === '0';
const turn = ([x, y, z]) => (V0 ? [-x, y, -z] : [x, y, z]);

// ---- binary buffer ----

const chunks = [];
let offset = 0;
const bufferViews = [];
const accessors = [];

function view(typed, target) {
  const bytes = new Uint8Array(typed.buffer, typed.byteOffset, typed.byteLength);
  const pad = (4 - (bytes.length % 4)) % 4;
  bufferViews.push({ buffer: 0, byteOffset: offset, byteLength: bytes.length, ...(target ? { target } : {}) });
  chunks.push(bytes, new Uint8Array(pad));
  offset += bytes.length + pad;
  return bufferViews.length - 1;
}

function vec3Accessor(points, withBounds, target) {
  const data = new Float32Array(points.flat());
  const accessor = { bufferView: view(data, target), componentType: 5126, count: points.length, type: 'VEC3' };
  if (withBounds) {
    accessor.min = [0, 1, 2].map((i) => Math.min(...points.map((p) => p[i])));
    accessor.max = [0, 1, 2].map((i) => Math.max(...points.map((p) => p[i])));
  }
  accessors.push(accessor);
  return accessors.length - 1;
}

function indexAccessor(indices) {
  accessors.push({ bufferView: view(new Uint16Array(indices), 34963), componentType: 5123, count: indices.length, type: 'SCALAR' });
  return accessors.length - 1;
}

// ---- a box ----

/** Eight corners of a box and its twelve triangles, counter-clockwise from outside. */
function corners([cx, cy, cz], [sx, sy, sz]) {
  const out = [];
  for (const z of [-1, 1]) for (const y of [-1, 1]) for (const x of [-1, 1]) out.push([cx + (x * sx) / 2, cy + (y * sy) / 2, cz + (z * sz) / 2]);
  return out;
}
const BOX_INDICES = [
  0, 2, 1, 1, 2, 3, // back
  4, 5, 6, 5, 7, 6, // front
  0, 1, 4, 1, 5, 4, // bottom
  2, 6, 3, 3, 6, 7, // top
  0, 4, 2, 2, 4, 6, // her right
  1, 3, 5, 3, 7, 5, // her left
];

// ---- materials ----

const materials = [
  { name: 'Skin', pbrMetallicRoughness: { baseColorFactor: [1.0, 0.86, 0.78, 1], metallicFactor: 0, roughnessFactor: 1 } },
  { name: 'Cloth', pbrMetallicRoughness: { baseColorFactor: [0.42, 0.36, 0.86, 1], metallicFactor: 0, roughnessFactor: 1 } },
  { name: 'Dark', pbrMetallicRoughness: { baseColorFactor: [0.16, 0.12, 0.24, 1], metallicFactor: 0, roughnessFactor: 1 } },
  { name: 'Hair', pbrMetallicRoughness: { baseColorFactor: [0.82, 0.62, 0.95, 1], metallicFactor: 0, roughnessFactor: 1 } },
  { name: 'White', pbrMetallicRoughness: { baseColorFactor: [1, 1, 1, 1], metallicFactor: 0, roughnessFactor: 1 } },
  { name: 'Mouth', pbrMetallicRoughness: { baseColorFactor: [0.72, 0.22, 0.32, 1], metallicFactor: 0, roughnessFactor: 1 } },
  // The colour of the skin, and used by one part only: an expression that binds a
  // material moves it once for every object that wears it.
  { name: 'Cheek', pbrMetallicRoughness: { baseColorFactor: [1.0, 0.86, 0.78, 1], metallicFactor: 0, roughnessFactor: 1 } },
];
const [SKIN, CLOTH, DARK, HAIR, WHITE, MOUTH, CHEEK] = [0, 1, 2, 3, 4, 5, 6];

// One unit cube shared by every plain box; a mesh per material.
const cubePositions = vec3Accessor(corners([0, 0, 0], [1, 1, 1]), true, 34962);
const cubeIndices = indexAccessor(BOX_INDICES);
const meshes = materials.map((m, i) => ({
  name: `Box_${m.name}`,
  primitives: [{ attributes: { POSITION: cubePositions }, indices: cubeIndices, material: i }],
}));

// ---- nodes ----

const nodes = [];
const parents = [];
function node(name, parent, translation, extra = {}) {
  nodes.push({ name, translation: turn(translation), ...extra });
  parents.push(parent);
  return nodes.length - 1;
}
const box = (name, parent, center, size, material) => node(name, parent, center, { scale: size, mesh: material });

const root = node('Root', null, [0, 0, 0]);
const hips = node('hips', root, [0, 0.9, 0]);
const spine = node('spine', hips, [0, 0.1, 0]);
const chest = node('chest', spine, [0, 0.15, 0]);
const neck = node('neck', chest, [0, 0.2, 0]);
const head = node('head', neck, [0, 0.07, 0]);
const leftEye = node('leftEye', head, [0.045, 0.11, 0]);
const rightEye = node('rightEye', head, [-0.045, 0.11, 0]);

const humanBones = { hips, spine, chest, neck, head, leftEye, rightEye };
for (const [side, s] of [['left', 1], ['right', -1]]) {
  const upperArm = node(`${side}UpperArm`, chest, [0.17 * s, 0.18, 0]);
  const lowerArm = node(`${side}LowerArm`, upperArm, [0.25 * s, 0, 0]);
  const hand = node(`${side}Hand`, lowerArm, [0.23 * s, 0, 0]);
  const upperLeg = node(`${side}UpperLeg`, hips, [0.08 * s, -0.05, 0]);
  const lowerLeg = node(`${side}LowerLeg`, upperLeg, [0, -0.37, 0]);
  const foot = node(`${side}Foot`, lowerLeg, [0, -0.4, 0]);
  Object.assign(humanBones, {
    [`${side}UpperArm`]: upperArm,
    [`${side}LowerArm`]: lowerArm,
    [`${side}Hand`]: hand,
    [`${side}UpperLeg`]: upperLeg,
    [`${side}LowerLeg`]: lowerLeg,
    [`${side}Foot`]: foot,
  });
  box(`${side}UpperArmBox`, upperArm, [0.125 * s, 0, 0], [0.25, 0.08, 0.08], CLOTH);
  box(`${side}LowerArmBox`, lowerArm, [0.115 * s, 0, 0], [0.23, 0.07, 0.07], SKIN);
  box(`${side}HandBox`, hand, [0.05 * s, 0, 0], [0.1, 0.07, 0.03], SKIN);
  box(`${side}UpperLegBox`, upperLeg, [0, -0.185, 0], [0.11, 0.37, 0.11], DARK);
  box(`${side}LowerLegBox`, lowerLeg, [0, -0.2, 0], [0.09, 0.4, 0.09], SKIN);
  box(`${side}FootBox`, foot, [0, -0.04, 0.04], [0.09, 0.08, 0.2], DARK);
  box(`${side}EyeWhite`, head, [0.045 * s, 0.11, 0.101], [0.036, 0.03, 0.004], WHITE);
  box(`${side}Pupil`, side === 'left' ? leftEye : rightEye, [0, 0, 0.106], [0.014, 0.018, 0.004], DARK);
}
box('PelvisBox', hips, [0, 0.03, 0], [0.26, 0.14, 0.14], CLOTH);
box('BellyBox', spine, [0, 0.07, 0], [0.24, 0.15, 0.13], CLOTH);
box('ChestBox', chest, [0, 0.1, 0], [0.3, 0.2, 0.15], CLOTH);
box('NeckBox', neck, [0, 0.035, 0], [0.07, 0.07, 0.07], SKIN);
box('HeadBox', head, [0, 0.1, 0], [0.2, 0.22, 0.2], SKIN);
box('HairBox', head, [0, 0.14, -0.03], [0.23, 0.2, 0.2], HAIR);

// ---- the face: parts that move ----

// Each part: boxes in the space of the head, and how each expression moves a corner.
const TARGETS = ['aa', 'ih', 'ou', 'ee', 'oh', 'blink', 'blinkLeft', 'blinkRight', 'happy', 'angry', 'sad', 'relaxed', 'surprised'];
const NARROW = { ou: 0.012, oh: 0.008, surprised: 0.006 };
const WIDE = { ih: 0.008, ee: 0.012 };
const OPEN = { aa: 0.03, ih: 0.008, ou: 0.015, ee: 0.012, oh: 0.025, surprised: 0.02 };

/** `p` is a corner at rest, `inner` says whether it is on the side of the nose. */
const PARTS = [
  {
    name: 'mouth',
    material: MOUTH,
    boxes: [[[0, 0.03, 0.105], [0.06, 0.012, 0.01]]],
    move: (t, p) => [
      (NARROW[t] ? -Math.sign(p[0]) * NARROW[t] : 0) + (WIDE[t] ? Math.sign(p[0]) * WIDE[t] : 0),
      p[1] < 0.03 && OPEN[t] ? -OPEN[t] : 0,
      0,
    ],
  },
  {
    name: 'mouthCorners',
    material: MOUTH,
    boxes: [[[0.038, 0.03, 0.105], [0.018, 0.012, 0.01]], [[-0.038, 0.03, 0.105], [0.018, 0.012, 0.01]]],
    move: (t, p) => [
      (NARROW[t] ? -Math.sign(p[0]) * NARROW[t] * 2 : 0) + (WIDE[t] ? Math.sign(p[0]) * WIDE[t] : 0),
      ({ happy: 0.012, sad: -0.012, angry: -0.006, relaxed: 0.005 })[t] ?? 0,
      0,
    ],
  },
  {
    name: 'lids',
    material: SKIN,
    boxes: [[[0.045, 0.142, 0.112], [0.042, 0.03, 0.006]], [[-0.045, 0.142, 0.112], [0.042, 0.03, 0.006]]],
    move: (t, p) => {
      const left = p[0] > 0;
      const down = { blink: 0.032, relaxed: 0.013, blinkLeft: left ? 0.032 : 0, blinkRight: left ? 0 : 0.032, surprised: -0.004 }[t] ?? 0;
      return [0, -down, 0];
    },
  },
  {
    name: 'cheeks',
    material: CHEEK,
    boxes: [[[0.062, 0.062, 0.1005], [0.04, 0.026, 0.003]], [[-0.062, 0.062, 0.1005], [0.04, 0.026, 0.003]]],
    move: () => [0, 0, 0],
  },
  {
    name: 'brows',
    material: DARK,
    boxes: [[[0.045, 0.17, 0.106], [0.042, 0.008, 0.008]], [[-0.045, 0.17, 0.106], [0.042, 0.008, 0.008]]],
    move: (t, p) => {
      const inner = Math.abs(p[0]) < 0.045;
      const up = { happy: 0.008, surprised: 0.02, angry: inner ? -0.016 : 0.004, sad: inner ? 0.014 : -0.004 }[t] ?? 0;
      return [0, up, 0];
    },
  },
];

const facePrimitives = PARTS.map((part) => {
  // `rest` is the face as it is drawn above; the file gets it turned for VRM 0.x.
  const rest = part.boxes.flatMap(([center, size]) => corners(center, size));
  const points = rest.map(turn);
  const indices = part.boxes.flatMap((_, i) => BOX_INDICES.map((v) => v + i * 8));
  return {
    attributes: { POSITION: vec3Accessor(points, true, 34962) },
    indices: indexAccessor(indices),
    material: part.material,
    targets: TARGETS.map((t) => ({ POSITION: vec3Accessor(rest.map((p) => turn(part.move(t, p))), true, 34962) })),
  };
});
meshes.push({ name: 'Face', primitives: facePrimitives, weights: TARGETS.map(() => 0), extras: { targetNames: TARGETS } });
const face = node('Face', head, [0, 0, 0], { mesh: meshes.length - 1 });

// ---- children ----

parents.forEach((parent, i) => {
  if (parent === null) return;
  (nodes[parent].children ??= []).push(i);
});

// ---- VRM ----

const bind = (name) => ({ morphTargetBinds: [{ node: face, index: TARGETS.indexOf(name), weight: 1 }] });
const vrm = {
  specVersion: '1.0',
  meta: {
    name: 'Пробная фигурка',
    version: '1',
    authors: ['Мика'],
    licenseUrl: 'https://vrm.dev/licenses/1.0/',
    avatarPermission: 'everyone',
    commercialUsage: 'personalNonProfit',
    allowExcessivelyViolentUsage: false,
    allowExcessivelySexualUsage: false,
    allowPoliticalOrReligiousUsage: false,
    allowAntisocialOrHateUsage: false,
    creditNotation: 'unnecessary',
    allowRedistribution: false,
    modification: 'prohibited',
  },
  humanoid: { humanBones: Object.fromEntries(Object.entries(humanBones).map(([name, index]) => [name, { node: index }])) },
  lookAt: {
    type: 'bone',
    offsetFromHeadBone: [0, 0.11, 0.1],
    rangeMapHorizontalInner: { inputMaxValue: 90, outputScale: 12 },
    rangeMapHorizontalOuter: { inputMaxValue: 90, outputScale: 12 },
    rangeMapVerticalDown: { inputMaxValue: 90, outputScale: 10 },
    rangeMapVerticalUp: { inputMaxValue: 90, outputScale: 10 },
  },
  expressions: {
    preset: Object.fromEntries(TARGETS.map((name) => [name, bind(name)])),
    // The model's own expression: the cheeks go pink.
    custom: { Blush: { materialColorBinds: [{ material: CHEEK, type: 'color', targetValue: [1.0, 0.45, 0.5, 1] }] } },
  },
};

// The same model as the older format describes it.
const OLD_NAMES = { happy: 'joy', angry: 'angry', sad: 'sorrow', relaxed: 'fun', aa: 'a', ih: 'i', ou: 'u', ee: 'e', oh: 'o', blink: 'blink', blinkLeft: 'blink_l', blinkRight: 'blink_r' };
const range = (yRange) => ({ curve: [0, 0, 0, 1, 1, 1, 1, 0], xRange: 90, yRange });
const group = (name, presetName, target) => ({
  name,
  presetName,
  binds: [{ mesh: meshes.length - 1, index: TARGETS.indexOf(target), weight: 100 }],
  materialValues: [],
  isBinary: false,
});
const vrm0 = {
  exporterVersion: 'mika make-sample-vrm',
  specVersion: '0.0',
  meta: {
    title: 'Пробная фигурка 0.x',
    version: '1',
    author: 'Мика',
    allowedUserName: 'Everyone',
    violentUssageName: 'Disallow',
    sexualUssageName: 'Disallow',
    commercialUssageName: 'Disallow',
    licenseName: 'Other',
  },
  humanoid: { humanBones: Object.entries(humanBones).map(([bone, index]) => ({ bone, node: index, useDefaultValues: true })) },
  firstPerson: {
    firstPersonBone: head,
    firstPersonBoneOffset: { x: 0, y: 0.11, z: 0 },
    meshAnnotations: [],
    lookAtTypeName: 'Bone',
    lookAtHorizontalInner: range(12),
    lookAtHorizontalOuter: range(12),
    lookAtVerticalDown: range(10),
    lookAtVerticalUp: range(10),
  },
  blendShapeMaster: {
    blendShapeGroups: [
      ...Object.entries(OLD_NAMES).map(([target, preset]) => group(target, preset, target)),
      // The older format has no standard surprise: it is the model's own.
      group('Surprised', 'unknown', 'surprised'),
      { name: 'Blush', presetName: 'unknown', binds: [], materialValues: [{ materialName: 'Cheek', propertyName: '_Color', targetValue: [1.0, 0.45, 0.5, 1] }], isBinary: false },
    ],
  },
  materialProperties: materials.map((m) => ({ name: m.name, shader: 'VRM_USE_GLTFSHADER', renderQueue: 2000, floatProperties: {}, vectorProperties: {}, textureProperties: {}, keywordMap: {}, tagMap: {} })),
};

const gltf = {
  asset: { version: '2.0', generator: 'mika make-sample-vrm' },
  extensionsUsed: [V0 ? 'VRM' : 'VRMC_vrm'],
  extensions: V0 ? { VRM: vrm0 } : { VRMC_vrm: vrm },
  scene: 0,
  scenes: [{ nodes: [root] }],
  nodes,
  meshes,
  materials,
  accessors,
  bufferViews,
  buffers: [{ byteLength: offset }],
};

// ---- the file ----

const json = new TextEncoder().encode(JSON.stringify(gltf));
const jsonPad = (4 - (json.length % 4)) % 4;
const total = 12 + 8 + json.length + jsonPad + 8 + offset;
const file = new Uint8Array(total);
const words = new DataView(file.buffer);
let at = 0;
const word = (value) => {
  words.setUint32(at, value, true);
  at += 4;
};
word(0x46546c67); // glTF
word(2);
word(total);
word(json.length + jsonPad);
word(0x4e4f534a); // JSON
file.set(json, at);
file.fill(0x20, at + json.length, at + json.length + jsonPad);
at += json.length + jsonPad;
word(offset);
word(0x004e4942); // BIN
for (const chunk of chunks) {
  file.set(chunk, at);
  at += chunk.length;
}

const target = fileURLToPath(new URL('./sample.vrm', import.meta.url));
writeFileSync(target, file);
console.log(`${target}: VRM ${V0 ? '0.x' : '1.0'}, ${total} bytes, ${nodes.length} nodes, ${TARGETS.length} expressions`);
