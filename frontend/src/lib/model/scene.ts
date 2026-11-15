// The living model: a three.js scene with a VRM model in it.
// The only file that imports `three` and `@pixiv/three-vrm`; it is loaded
// with a dynamic `import()` and only when a model is chosen.
//
// One scene per window. Its canvas moves between the views that show her:
// the view that loses it gets its last frame as a picture.

import { VRMLoaderPlugin, VRMUtils, type VRM, type VRMHumanBoneName } from '@pixiv/three-vrm';
import { invoke } from '@tauri-apps/api/core';
import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { EMOTION_ORDER, MOUTH_STEP_MS, blinkPause, gesturePause, type Emotion } from '../avatar';
import {
  EMOTION_BLEND_MS,
  LIP_TAU_MS,
  MAX_PIXEL_RATIO,
  VOWELS,
  approach,
  blinkWeight,
  boneAngles,
  breath,
  canModelBlink,
  frameDue,
  frameFor,
  gazeWander,
  gestureAt,
  lipGain,
  nextVowel,
  rigSign,
  shouldRender,
  slotPose,
  slotWeights,
  type Pose,
  type Profile,
  type Vowel,
} from './layers';
import { FULL_STILL, stillName } from './show';

/** Why a scene could not be shown: no WebGL, a model that did not load, a lost context. */
export type FailKind = 'webgl' | 'load' | 'context_lost';

export class SceneError extends Error {
  constructor(
    readonly kind: FailKind,
    message: string,
  ) {
    super(message);
  }
}

export interface SceneInput {
  emotion: Emotion;
  talking: boolean;
  /** The window of this page is seen. */
  windowShown: boolean;
  /** The system asks for less motion: the face lives, the body stands. */
  still: boolean;
}

export interface Scene {
  /** Moves the canvas into `host`. The view that had it gets `onLeave` with its last frame as a data URL. */
  attach(host: HTMLElement, hooks: { onLeave: (lastFrame: string) => void; onFail: (kind: FailKind) => void }): void;
  /** Gives the canvas up if `host` still has it. */
  detach(host: HTMLElement): void;
  set(input: SceneInput): void;
  setProfile(profile: Profile): void;
  gesture(kind: Emotion | 'poke'): void;
  /** Draws the 21 pictures of the model: PNG data URLs by name, 256x256 chips and one 384x512 full frame. */
  stills(): Map<string, string>;
  /** Names the loaded model really has. */
  readonly expressions: ReadonlySet<string>;
  dispose(): void;
}

const rad = (deg: number) => (deg * Math.PI) / 180;
/** Vertical field of view of the camera, degrees: narrow, so the figure is not distorted. */
const FOV = 20;
/** The emotion reaches 95% of its weight in `EMOTION_BLEND_MS`. */
const EMOTION_TAU = EMOTION_BLEND_MS / 3;
const GAZE_TAU = 90;

class LiveScene implements Scene {
  readonly expressions: ReadonlySet<string>;

  private readonly canvas: HTMLCanvasElement;
  private readonly camera = new THREE.PerspectiveCamera(FOV, 3 / 4, 0.1, 20);
  private readonly world = new THREE.Scene();
  private host: HTMLElement | null = null;
  private hooks: { onLeave: (lastFrame: string) => void; onFail: (kind: FailKind) => void } | null = null;
  private readonly observer = new ResizeObserver(() => this.resize());
  private raf = 0;
  private last = 0;
  private input: SceneInput = { emotion: 'neutral', talking: false, windowShown: true, still: false };

  /** Turns about X and Z go the other way on a VRM 0.x rig. */
  private readonly sign: 1 | -1;
  // Bones of the normalized rig: every one of them rests at zero rotation.
  private readonly bones: Partial<Record<VRMHumanBoneName, THREE.Object3D>> = {};
  private hipsRest = 0;
  private height = 1.5;
  private frame = { headY: 1.4, hipsY: 0.9, topY: 1.55 };

  private readonly emo = new Map<string, number>();
  private readonly touched = new Set<string>();
  private readonly lips: Record<Vowel, number> = { aa: 0, ih: 0, ou: 0, ee: 0, oh: 0 };
  private vowel: Vowel | null = null;
  private vowelAmp = 1;
  private vowelAt = 0;
  private blinkAt = 0;
  private readonly pose: Pose = { pitch: 0, yaw: 0, roll: 0, spine: 0 };
  private readonly gaze: [number, number] = [0, 0];
  private wander: { yaw: number; pitch: number } | null = null;
  private wanderAt = 0;
  private gestureKind: Emotion | 'poke' | null = null;
  private gestureStart = 0;

  constructor(
    private readonly renderer: THREE.WebGLRenderer,
    private readonly vrm: VRM,
    private profile: Profile,
  ) {
    this.canvas = renderer.domElement;
    this.canvas.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;display:block;pointer-events:none';
    this.canvas.addEventListener('webglcontextlost', (e) => {
      e.preventDefault();
      this.hooks?.onFail('context_lost');
    });
    this.expressions = new Set(Object.keys(vrm.expressionManager?.expressionMap ?? {}));
    this.sign = rigSign(vrm.meta?.metaVersion);

    this.world.add(vrm.scene);
    this.world.add(new THREE.AmbientLight(0xffffff, 0.6));
    const sun = new THREE.DirectionalLight(0xffffff, Math.PI);
    sun.position.set(0.6, 1, 1.2);
    this.world.add(sun);

    for (const name of ['hips', 'spine', 'chest', 'head', 'leftUpperArm', 'rightUpperArm', 'leftLowerArm', 'rightLowerArm'] as const) {
      const node = vrm.humanoid.getNormalizedBoneNode(name);
      if (node) this.bones[name] = node;
    }
    this.restPose();
    this.measure();
    const now = performance.now();
    this.blinkAt = now + blinkPause(Math.random());
    this.wanderAt = now + gesturePause(Math.random());
  }

  /** A model without animation stands with its arms out: bring them down. */
  private restPose() {
    const { leftUpperArm, rightUpperArm, leftLowerArm, rightLowerArm, hips } = this.bones;
    const at = boneAngles(slotPose('neutral'), 0, 0, this.sign);
    if (leftUpperArm) leftUpperArm.rotation.z = rad(at.leftUpperArmZ);
    if (rightUpperArm) rightUpperArm.rotation.z = rad(at.rightUpperArmZ);
    if (leftLowerArm) leftLowerArm.rotation.y = rad(at.leftLowerArmY);
    if (rightLowerArm) rightLowerArm.rotation.y = rad(at.rightLowerArmY);
    this.hipsRest = hips?.position.y ?? 0;
  }

  /** Where her head and hips are, for the camera. */
  private measure() {
    this.vrm.update(0);
    this.vrm.scene.updateMatrixWorld(true);
    const box = new THREE.Box3().setFromObject(this.vrm.scene);
    const at = new THREE.Vector3();
    const y = (node: THREE.Object3D | undefined, fallback: number) => (node ? node.getWorldPosition(at).y : fallback);
    const top = Number.isFinite(box.max.y) ? box.max.y : 1.55;
    const bottom = Number.isFinite(box.min.y) ? box.min.y : 0;
    this.height = Math.max(0.3, top - bottom);
    this.frame = {
      headY: y(this.bones.head, bottom + this.height * 0.9),
      hipsY: y(this.bones.hips, bottom + this.height * 0.55),
      topY: top,
    };
  }

  private aim(view: 'full' | 'head', aspect: number) {
    const f = frameFor(view, this.frame.headY, this.frame.hipsY, this.frame.topY);
    const distance = f.height / 2 / Math.tan(rad(FOV) / 2);
    this.camera.aspect = aspect;
    this.camera.position.set(0, f.centerY, distance);
    this.camera.lookAt(0, f.centerY, 0);
    this.camera.updateProjectionMatrix();
  }

  private resize() {
    if (!this.host) return;
    const w = Math.max(1, this.host.clientWidth);
    const h = Math.max(1, this.host.clientHeight);
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, MAX_PIXEL_RATIO));
    this.renderer.setSize(w, h, false);
    this.aim('full', w / h);
  }

  attach(host: HTMLElement, hooks: { onLeave: (lastFrame: string) => void; onFail: (kind: FailKind) => void }) {
    if (this.host === host) {
      this.hooks = hooks;
      return;
    }
    if (this.host && this.hooks) {
      // The view that had her keeps a picture of her while it fades away.
      this.renderer.render(this.world, this.camera);
      this.hooks.onLeave(this.canvas.toDataURL('image/png'));
      this.observer.unobserve(this.host);
    }
    this.host = host;
    this.hooks = hooks;
    // The graphics may have gone while no view held her: nobody was there to hear of it.
    if (this.renderer.getContext().isContextLost()) {
      hooks.onFail('context_lost');
      return;
    }
    host.appendChild(this.canvas);
    this.observer.observe(host);
    this.resize();
    if (!this.raf) {
      this.last = performance.now();
      this.raf = requestAnimationFrame(this.tick);
    }
  }

  detach(host: HTMLElement) {
    if (this.host !== host) return;
    this.observer.unobserve(host);
    this.canvas.remove();
    this.host = null;
    this.hooks = null;
    cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  set(input: SceneInput) {
    this.input = input;
  }

  setProfile(profile: Profile) {
    this.profile = profile;
  }

  gesture(kind: Emotion | 'poke') {
    this.gestureKind = kind;
    this.gestureStart = performance.now();
  }

  private readonly tick = (now: number) => {
    this.raf = requestAnimationFrame(this.tick);
    if (!shouldRender({ attached: this.host !== null, windowShown: this.input.windowShown, documentHidden: document.hidden })) {
      this.last = now;
      return;
    }
    if (!frameDue(now, this.last)) return;
    // After a pause she does not leap: a step is never longer than a tenth of a second.
    const dt = Math.min(100, now - this.last);
    this.last = now;
    try {
      this.step(now, dt);
      this.renderer.render(this.world, this.camera);
    } catch {
      // A model that breaks a frame would break every frame: she goes back to the pictures.
      cancelAnimationFrame(this.raf);
      this.raf = 0;
      this.hooks?.onFail('load');
    }
  };

  /** One frame of her life, layer by layer. */
  private step(now: number, dt: number) {
    const { emotion, talking, still } = this.input;
    const slot = this.profile.slots[emotion];

    // The emotion comes in and the previous one goes out.
    const targets = slotWeights(slot, this.expressions);
    for (const name of new Set([...this.emo.keys(), ...targets.keys()])) {
      const value = approach(this.emo.get(name) ?? 0, targets.get(name) ?? 0, dt, EMOTION_TAU);
      if (value < 0.001 && !targets.has(name)) this.emo.delete(name);
      else this.emo.set(name, value);
    }

    // Blinking, unless the face itself closes the eyes.
    let blink = 0;
    if (!canModelBlink(slot)) {
      this.blinkAt = Math.max(this.blinkAt, now + 400);
    } else if (now >= this.blinkAt) {
      blink = blinkWeight(now - this.blinkAt);
      if (now - this.blinkAt >= 190) this.blinkAt = now + blinkPause(Math.random());
    }

    // Lips while she talks.
    if (!talking) {
      this.vowel = null;
    } else if (now >= this.vowelAt) {
      this.vowel = nextVowel(this.vowel, Math.random());
      this.vowelAmp = 0.6 + 0.4 * Math.random();
      this.vowelAt = now + MOUTH_STEP_MS;
    }
    const gain = lipGain(this.profile, emotion);
    for (const v of VOWELS) {
      this.lips[v] = approach(this.lips[v], this.vowel === v ? Math.min(1, gain * this.vowelAmp) : 0, dt, LIP_TAU_MS);
    }

    const out = new Map(this.emo);
    if (this.expressions.has('blink')) out.set('blink', Math.max(out.get('blink') ?? 0, blink));
    for (const v of VOWELS) if (this.expressions.has(v)) out.set(v, Math.max(out.get(v) ?? 0, this.lips[v]));
    this.applyExpressions(out);

    // Head and spine of the emotion, breathing, a one-off gesture.
    const want = slotPose(emotion);
    for (const key of ['pitch', 'yaw', 'roll', 'spine'] as const) {
      this.pose[key] = approach(this.pose[key], want[key], dt, EMOTION_TAU);
    }
    let move: { y: number; roll: number } | null = null;
    if (this.gestureKind && !still) {
      move = gestureAt(this.gestureKind, now - this.gestureStart);
      if (!move) this.gestureKind = null;
    }
    this.applyPose(this.pose, still ? 0 : breath(now), move);

    // The eyes: where the emotion looks, or now and then aside.
    if (now >= this.wanderAt) {
      if (this.wander) {
        this.wander = null;
        this.wanderAt = now + gesturePause(Math.random());
      } else {
        const glance = gazeWander(Math.random(), Math.random());
        this.wander = glance;
        this.wanderAt = now + glance.ms;
      }
    }
    const look = slot.gaze ?? (this.wander && !still ? [this.wander.yaw, this.wander.pitch] : [0, 0]);
    this.gaze[0] = approach(this.gaze[0], look[0], dt, GAZE_TAU);
    this.gaze[1] = approach(this.gaze[1], look[1], dt, GAZE_TAU);
    this.applyGaze(this.gaze[0], this.gaze[1]);

    this.vrm.update(dt / 1000);
  }

  private applyExpressions(weights: ReadonlyMap<string, number>) {
    const manager = this.vrm.expressionManager;
    if (!manager) return;
    for (const name of this.touched) if (!weights.has(name)) manager.setValue(name, 0);
    for (const [name, weight] of weights) {
      manager.setValue(name, weight);
      this.touched.add(name);
    }
  }

  private applyPose(pose: Pose, breathing: number, move: { y: number; roll: number } | null) {
    const { head, spine, chest, hips } = this.bones;
    const at = boneAngles(pose, breathing, move?.roll ?? 0, this.sign);
    head?.rotation.set(rad(at.head[0]), rad(at.head[1]), rad(at.head[2]));
    // A model without a chest bone breathes with its spine.
    if (spine) spine.rotation.x = rad(at.spineX + (chest ? 0 : at.chestX));
    if (chest) chest.rotation.x = rad(at.chestX);
    if (hips) {
      hips.position.y = this.hipsRest + (move?.y ?? 0) * this.height;
      hips.rotation.z = rad(at.hipsZ);
    }
  }

  /** Yaw to her left and pitch up are positive; the format counts pitch down. */
  private applyGaze(yaw: number, pitch: number) {
    const lookAt = this.vrm.lookAt;
    if (!lookAt) return;
    lookAt.yaw = yaw;
    lookAt.pitch = -pitch;
  }

  stills(): Map<string, string> {
    const out = new Map<string, string>();
    const shown = this.canvas.style.visibility;
    // The canvas changes its size 21 times: nobody should see that.
    this.canvas.style.visibility = 'hidden';
    const manager = this.vrm.expressionManager;
    const shot = (view: 'full' | 'head', w: number, h: number, emotion: Emotion, open: boolean) => {
      const slot = this.profile.slots[emotion];
      const weights = slotWeights(slot, this.expressions);
      if (open && this.expressions.has('aa')) {
        weights.set('aa', Math.max(weights.get('aa') ?? 0, Math.min(1, 0.7 * lipGain(this.profile, emotion))));
      }
      manager?.resetValues();
      for (const [name, weight] of weights) manager?.setValue(name, weight);
      this.applyPose(slotPose(emotion), 0, null);
      this.applyGaze(slot.gaze?.[0] ?? 0, slot.gaze?.[1] ?? 0);
      this.vrm.update(0);
      this.renderer.setPixelRatio(1);
      this.renderer.setSize(w, h, false);
      this.aim(view, w / h);
      this.renderer.render(this.world, this.camera);
      return this.canvas.toDataURL('image/png');
    };
    try {
      for (const emotion of EMOTION_ORDER) {
        for (const open of [false, true]) out.set(stillName(emotion, open), shot('head', 256, 256, emotion, open));
      }
      out.set(FULL_STILL, shot('full', 384, 512, 'neutral', false));
    } finally {
      manager?.resetValues();
      this.emo.clear();
      this.touched.clear();
      this.resize();
      this.canvas.style.visibility = shown;
    }
    return out;
  }

  dispose() {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.observer.disconnect();
    this.canvas.remove();
    this.host = null;
    this.hooks = null;
    VRMUtils.deepDispose(this.vrm.scene);
    this.renderer.dispose();
    // A page may hold only so many graphics contexts: this one is given back now, not when memory is swept.
    this.renderer.forceContextLoss();
  }
}

/** A model file may only use what is inside it: any other address is refused. */
function guardedLoader(): { loader: GLTFLoader; blocked: () => boolean } {
  let blocked = false;
  const manager = new THREE.LoadingManager();
  manager.setURLModifier((url) => {
    if (/^(data|blob):/i.test(url)) return url;
    blocked = true;
    return 'data:,';
  });
  const loader = new GLTFLoader(manager);
  loader.register((parser) => new VRMLoaderPlugin(parser));
  return { loader, blocked: () => blocked };
}

async function load(id: string, profile: Profile): Promise<LiveScene> {
  let renderer: THREE.WebGLRenderer;
  try {
    renderer = new THREE.WebGLRenderer({ alpha: true, antialias: true, premultipliedAlpha: true });
    renderer.setClearColor(0x000000, 0);
  } catch (e) {
    throw new SceneError('webgl', String(e));
  }
  try {
    const bytes = await invoke<ArrayBuffer>('avatar_model_bytes', { id });
    const { loader, blocked } = guardedLoader();
    const gltf = await loader.parseAsync(bytes, '');
    const vrm = gltf.userData.vrm as VRM | undefined;
    if (!vrm) throw new Error('no VRM data in the file');
    if (blocked()) throw new Error('the model refers to outside files');
    VRMUtils.removeUnnecessaryVertices(gltf.scene);
    VRMUtils.combineSkeletons(gltf.scene);
    if (vrm.meta?.metaVersion === '0') VRMUtils.rotateVRM0(vrm);
    // Skinned parts move outside the boxes they were given at rest.
    vrm.scene.traverse((node) => {
      node.frustumCulled = false;
    });
    return new LiveScene(renderer, vrm, profile);
  } catch (e) {
    renderer.dispose();
    throw e instanceof SceneError ? e : new SceneError('load', String(e));
  }
}

let current: { id: string; scene: Promise<LiveScene> } | null = null;

/** The scene of this window for model `id`, loaded once; a second call returns the same scene with the newer profile. */
export function sceneFor(id: string, profile: Profile): Promise<Scene> {
  if (current?.id !== id) {
    dropScene();
    const made = load(id, profile);
    current = { id, scene: made };
    made.catch(() => {
      if (current?.scene === made) current = null;
    });
    return made;
  }
  return current.scene.then((scene) => {
    scene.setProfile(profile);
    return scene;
  });
}

/** Drops the scene of this window: another model was chosen, or it failed. */
export function dropScene() {
  const old = current;
  current = null;
  old?.scene.then((scene) => scene.dispose()).catch(() => {});
}
