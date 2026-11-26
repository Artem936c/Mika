// The living model: a three.js scene with a VRM model in it.
// The only file that imports `three` and `@pixiv/three-vrm`; it is loaded
// with a dynamic `import()` and only when a model is chosen.
//
// One scene per window. Its canvas moves between the views that show her:
// the view that loses it gets its last frame as a picture.

import { VRMLoaderPlugin, VRMUtils, type VRM, type VRMHumanBoneName } from '@pixiv/three-vrm';
import { VRMAnimationLoaderPlugin, createVRMAnimationHumanoidTracks, type VRMAnimation } from '@pixiv/three-vrm-animation';
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
  breath,
  canModelBlink,
  frameDue,
  frameFor,
  gazeWander,
  gestureAt,
  glide,
  lipGain,
  nextVowel,
  rigSign,
  shouldRender,
  slotPose,
  slotWeights,
  trunkPose,
  type ModelFrame,
  type Pose,
  type Profile,
  type Vowel,
} from './layers';
import { fingerAxes, fingerTurn, palmOf, type FingerAxes, type Vec } from './fingers';
import { motionOf, toRig, type Quat } from './idle';
import { smoothOpen } from './lips';
import {
  BODY_TAU_MS,
  EMOTION_BODY,
  END_CUES,
  GESTURES,
  IDLE_ACTIONS,
  STATE_BODY,
  autoPosePause,
  beatOffset,
  beats,
  bodyState,
  endCue,
  idleActionPause,
  idleOffset,
  limitHead,
  nextAutoPose,
  autoPoseNow,
  saccade,
  sampleTimeline,
  windowLean,
  speechStrength,
  twiceBlink,
  ZERO_TRUNK,
  type IdleAction,
  type Timeline,
} from './body';
import { REST_POSES, restPoseOf, type RestPose } from './poses';
import { RIG_BONES, add, mix, withSign, type RigBone, type RigPose } from './rig';
import { FULL_STILL, stillName } from './show';
import { STATE_GAZE, aimAt, angleTo, cursorInterest, pickGaze } from './gaze';
import { mannerOf, reactionFor, restingEmotion } from './manner';
import { Probe, REACTION, nearestBone, partOf, type TouchPart } from './touch';
import { currentPhrase, mouthNow, voice } from './voice.svelte';

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
  /** Her state and whether a tool is at work. */
  state?: string;
  working?: boolean;
  /** `performance.now()` of the last message the user sent: she nods to it. */
  sentAt?: number;
  /** The rest pose from the settings, or `auto`. */
  pose?: string;
  /** Whose manner her body takes: the id of the character in force (decision 0055). */
  persona?: string;
  /** Idle actions now and then. */
  idleActions?: boolean;
  /** The user's animation she loops at rest instead of the rest pose. */
  idleMotion?: string | null;
  /** What part of her the view shows. */
  frame?: 'full' | 'waist' | 'close';
  /** She looks at the cursor. */
  follow?: boolean;
  /** The cursor in pixels of the window, and when it last moved. */
  cursor?: { x: number; y: number; movedAt: number } | null;
  /** Where the user types, in pixels of the window, and when. */
  typing?: { x: number; y: number; at: number } | null;
}

export interface Scene {
  /** Moves the canvas into `host`. The view that had it gets `onLeave` with its last frame as a data URL. */
  attach(
    host: HTMLElement,
    hooks: {
      onLeave: (lastFrame: string) => void;
      onFail: (kind: FailKind) => void;
    },
  ): void;
  /** Gives the canvas up if `host` still has it. */
  detach(host: HTMLElement): void;
  set(input: SceneInput): void;
  setProfile(profile: Profile): void;
  gesture(kind: Emotion | 'poke'): void;
  /** The part of her under a point of the window; null when the point misses her. */
  touch(clientX: number, clientY: number): TouchPart | null;
  /** Her body answers a click on `part`, after the face it brought when known. */
  react(part: TouchPart, emotion?: Emotion): void;
  /** How opaque she is under a point of the window, 0..255, as of the last frame drawn. */
  /** Asks how opaque she is under a point of the window, 0..255; the answer comes after her next frame. */
  probe(clientX: number, clientY: number, answer: (alpha: number) => void): void;
  /** The window moves at this speed, logical pixels per second: her body and hair lag behind. */
  push(vx: number, vy: number): void;
  /** Plays one of her gestures by name, or an animation of the user by its id. */
  perform(gesture: { name: string; motion: string | null }): void;
  /** Draws the 21 pictures of the model: PNG data URLs by name, 256x256 chips and one 384x512 full frame. */
  stills(): Map<string, string>;
  /** Names the loaded model really has. */
  readonly expressions: ReadonlySet<string>;
  dispose(): void;
}

const rad = (deg: number) => (deg * Math.PI) / 180;
/** The fingers of the relaxed pose: they stay at ease under an idle animation. */
const FINGERS_AT_EASE: RigPose = Object.fromEntries(
  Object.entries(REST_POSES.relaxed).filter(([bone]) => /Thumb|Index|Middle|Ring|Little/.test(bone)),
);
/** Vertical field of view of the camera, degrees: narrow, so the figure is not distorted. */
const FOV = 20;
/** The emotion reaches 95% of its weight in `EMOTION_BLEND_MS`. */
const EMOTION_TAU = EMOTION_BLEND_MS / 3;
/** How the gaze glides (decision 0040): about the time to get there, ms, and the top speed, degrees a second. */
const HEAD_GLIDE_MS = 170;
const HEAD_MAX_SPEED = 180;
const BODY_GLIDE_MS = 320;
const BODY_MAX_SPEED = 60;
const EYES_GLIDE_MS = 70;
const EYES_MAX_SPEED = 400;
/** A small eye jump takes about this long, ms. */
const JUMP_TAU = 20;
/** A cursor that moved this recently is pursued, ms. */
const PURSUIT_MS = 400;

class LiveScene implements Scene {
  readonly expressions: ReadonlySet<string>;

  private readonly canvas: HTMLCanvasElement;
  private readonly camera = new THREE.PerspectiveCamera(FOV, 3 / 4, 0.1, 20);
  private readonly world = new THREE.Scene();
  private host: HTMLElement | null = null;
  private hooks: {
    onLeave: (lastFrame: string) => void;
    onFail: (kind: FailKind) => void;
  } | null = null;
  private readonly observer = new ResizeObserver(() => this.resize());
  private raf = 0;
  private last = 0;
  private input: SceneInput = {
    emotion: 'neutral',
    talking: false,
    windowShown: true,
    still: false,
  };

  /** Turns about X and Z go the other way on a VRM 0.x rig. */
  private readonly sign: 1 | -1;
  // Bones of the normalized rig: every one of them rests at zero rotation.
  private readonly bones: Partial<Record<RigBone, THREE.Object3D>> = {};
  private readonly euler = new THREE.Euler();
  /** Axes of every finger joint of this model, in the space of the joint (decision 0041). */
  private readonly fingers = new Map<RigBone, { axes: FingerAxes; side: 'left' | 'right' }>();
  /** A rest pose set by hand in the preview; null: the one of the settings. */
  private restOverride: RigPose | null = null;
  /** The body as it is now, on its way to the pose it is asked for. */
  private body: RigPose = { ...ZERO_TRUNK, ...REST_POSES.relaxed };
  /** Makes the slow life of this window differ from another's. */
  private readonly seed = Math.random();
  /** The rest pose of `auto` and when it changes next. */
  private autoRest: RestPose = 'relaxed';
  private autoAt = 0;
  /** One-off movements playing now: idle actions, end of a phrase, a nod. */
  private moves: { tl: Timeline; start: number }[] = [];
  private actionAt = 0;
  private lastSent = -Infinity;
  /** The phrase her hands follow, its beats and the beat playing. */
  private phraseAt = -1;
  private phraseText = '';
  private beatTimes: number[] = [];
  private beatIndex = 0;
  private beat: { start: number; side: 'left' | 'right' } | null = null;
  private beatSide: 'left' | 'right' = 'left';
  private hipsRest = 0;
  private hipsRestX = 0;
  private hipsRestZ = 0;
  private height = 1.5;
  private frame = { headY: 1.4, hipsY: 0.9, topY: 1.55 };

  private readonly emo = new Map<string, number>();
  private readonly touched = new Set<string>();
  private readonly lips: Record<Vowel, number> = {
    aa: 0,
    ih: 0,
    ou: 0,
    ee: 0,
    oh: 0,
  };
  private vowel: Vowel | null = null;
  private vowelAmp = 1;
  private vowelAt = 0;
  /** How far the mouth is open while the voice sounds, 0..1. */
  private opening = 0;
  private blinkAt = 0;
  private readonly pose: Pose = { pitch: 0, yaw: 0, roll: 0, spine: 0 };
  private wander: { yaw: number; pitch: number } | null = null;
  private wanderAt = 0;
  /** Animations of the user, loaded once, and the one playing over the layers. */
  private readonly clips = new Map<string, THREE.AnimationClip>();
  private mixer: THREE.AnimationMixer | null = null;
  private clip: {
    action: THREE.AnimationAction;
    start: number;
    ms: number;
    nodes: THREE.Object3D[];
  } | null = null;
  private readonly held = new THREE.Quaternion();
  /** The animation she loops at rest (decision 0041): asked for, and loaded. */
  private idleWant: string | null = null;
  private idle: {
    ms: number;
    tracks: [RigBone, THREE.Interpolant][];
    /** Its first frame: stands for the rest pose; the rest of it is added as motion. */
    first: RigPose;
  } | null = null;
  /** The point to read the opacity of after the next frame, and what was read. */
  private readonly prober = new Probe();
  private readonly pixel = new Uint8Array(4);
  /** Where each bone of the clip stands at rest: what a clip blends from and goes back to. */
  private readonly restOf = new Map<THREE.Object3D, { q: THREE.Quaternion; p: THREE.Vector3 }>();
  private readonly blendQ = new THREE.Quaternion();
  private readonly blendP = new THREE.Vector3();
  /** The lean of a moving window, degrees, and the shift that swings her hair. */
  private readonly drift = { lean: 0, shift: 0, vx: 0 };
  /** How far her head and body are turned to what she looks at, degrees. */
  /** Head and body turn to what she looks at, gliding: degrees, degrees a millisecond. */
  private readonly turn = { yaw: { x: 0, v: 0 }, pitch: { x: 0, v: 0 }, body: { x: 0, v: 0 } };
  /** Her eyes glide too, so a cursor that moves in steps does not shake them. */
  private readonly eyes = { yaw: { x: 0, v: 0 }, pitch: { x: 0, v: 0 } };
  /** The small eye jump as shown: it moves fast, but never in one frame. */
  private readonly jumpNow = { yaw: 0, pitch: 0 };
  /** She follows a cursor that moves right now: the eyes pursue it without jumps. */
  private pursuing = false;
  private readonly at = new THREE.Vector3();
  /** A small jump of the eyes on top of where they look. */
  private jump = { yaw: 0, pitch: 0 };
  private jumpAt = 0;
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

    for (const name of RIG_BONES) {
      const node = vrm.humanoid.getNormalizedBoneNode(name as VRMHumanBoneName);
      if (node) this.bones[name] = node;
    }
    this.hipsRest = this.bones.hips?.position.y ?? 0;
    this.hipsRestX = this.bones.hips?.position.x ?? 0;
    this.hipsRestZ = this.bones.hips?.position.z ?? 0;
    this.measureFingers();
    this.applyRig(this.body);
    this.measure();
    const now = performance.now();
    this.blinkAt = now + blinkPause(Math.random());
    this.wanderAt = now + gesturePause(Math.random());
    this.actionAt = now + idleActionPause(Math.random());
    this.autoAt = now + autoPosePause(Math.random());
  }

  /**
   * The axes of every finger joint, from where the joints stand in the
   * T-pose (decision 0041). The rig is at rest now, so the world turn of a
   * joint is the turn of its parent: an axis in the world becomes an axis
   * of the joint through it.
   */
  private measureFingers() {
    this.vrm.scene.updateMatrixWorld(true);
    const at = (bone: RigBone): Vec | null => {
      const node = this.bones[bone];
      if (!node) return null;
      node.getWorldPosition(this.at);
      return [this.at.x, this.at.y, this.at.z];
    };
    const q = new THREE.Quaternion();
    const axis = new THREE.Vector3();
    const local = (node: THREE.Object3D, v: Vec): Vec => {
      node.getWorldQuaternion(q).invert();
      axis.set(v[0], v[1], v[2]).applyQuaternion(q);
      return [axis.x, axis.y, axis.z];
    };
    for (const side of ['left', 'right'] as const) {
      const hand = at(`${side}Hand`);
      const index = at(`${side}IndexProximal`);
      const little = at(`${side}LittleProximal`);
      if (!hand || !index || !little) continue;
      const palm = palmOf(hand, index, little, [0, -1, 0]);
      for (const chain of [
        ['ThumbMetacarpal', 'ThumbProximal', 'ThumbDistal'],
        ['IndexProximal', 'IndexIntermediate', 'IndexDistal'],
        ['MiddleProximal', 'MiddleIntermediate', 'MiddleDistal'],
        ['RingProximal', 'RingIntermediate', 'RingDistal'],
        ['LittleProximal', 'LittleIntermediate', 'LittleDistal'],
      ]) {
        const bones = chain.map((b) => `${side}${b}` as RigBone);
        const points = bones.map(at);
        points.forEach((joint, i) => {
          const node = this.bones[bones[i]];
          if (!joint || !node) return;
          // The last joint goes on the way the one before it went.
          const prev = points[i - 1];
          const next = points[i + 1] ?? (prev ? ([2 * joint[0] - prev[0], 2 * joint[1] - prev[1], 2 * joint[2] - prev[2]] as Vec) : null);
          if (!next) return;
          const world = fingerAxes(joint, next, palm);
          this.fingers.set(bones[i], {
            axes: { flex: local(node, world.flex), spread: local(node, world.spread), twist: local(node, world.twist) },
            side,
          });
        });
      }
    }
  }

  /** Turns every bone of the rig to `pose`; a bone it leaves out goes back to rest. */
  private applyRig(pose: RigPose) {
    const signed = withSign(pose, this.sign);
    for (const [bone, node] of Object.entries(this.bones) as [RigBone, THREE.Object3D][]) {
      const finger = this.fingers.get(bone);
      if (finger) {
        const [x, y, z, w] = fingerTurn(finger.axes, pose[bone] ?? [0, 0, 0], finger.side);
        node.quaternion.set(x, y, z, w);
        continue;
      }
      const [x, y, z] = signed[bone] ?? [0, 0, 0];
      this.euler.set(rad(x), rad(y), rad(z), 'XYZ');
      node.quaternion.setFromEuler(this.euler);
    }
  }

  /** Puts her in `pose` whatever the settings say; null gives the settings back. For the preview. */
  setRest(pose: RigPose | null) {
    this.restOverride = pose;
    if (pose) this.body = { ...ZERO_TRUNK, ...pose };
  }

  /** Loads the animation she loops at rest when another one is asked for; a file that fails leaves the rest pose. */
  private wantIdle() {
    const want = this.input.idleMotion || null;
    if (want === this.idleWant) return;
    this.idleWant = want;
    this.idle = null;
    if (!want) return;
    this.loadClip(want)
      .then((clip) => {
        if (this.idleWant !== want) return;
        const byName = new Map<string, RigBone>();
        for (const [bone, node] of Object.entries(this.bones) as [RigBone, THREE.Object3D][]) byName.set(node.name, bone);
        const tracks: [RigBone, THREE.Interpolant][] = [];
        for (const track of clip.tracks) {
          if (!track.name.endsWith('.quaternion')) continue;
          const bone = byName.get(track.name.slice(0, -'.quaternion'.length));
          if (bone) tracks.push([bone, new THREE.QuaternionLinearInterpolant(track.times, track.values, 4, new Float32Array(4))]);
        }
        const ms = clip.duration * 1000;
        if (!tracks.length || !(ms > 0)) return;
        this.idle = { ms, tracks, first: this.idleAt(0, ms, tracks) };
      })
      .catch(() => {
        // An animation that does not load is not looped; the rest pose stays.
      });
  }

  /** The idle animation of `length` ms at `ms` from its start, as a pose. */
  private idleAt(ms: number, length: number, tracks: [RigBone, THREE.Interpolant][]): RigPose {
    const t = (((ms % length) + length) % length) / 1000;
    const frame: [RigBone, Quat][] = tracks.map(([bone, interpolant]) => {
      const v = interpolant.evaluate(t);
      return [bone, [v[0], v[1], v[2], v[3]]];
    });
    return toRig(frame, this.sign);
  }

  /** The rest pose she stands in now. */
  private restNow(now: number): RigPose {
    if (this.restOverride) return this.restOverride;
    // An idle animation stands for the rest pose; the fingers stay at ease.
    if (this.idle) return { ...FINGERS_AT_EASE, ...this.idle.first };
    const name = this.input.pose ?? 'relaxed';
    if (name !== 'auto') return restPoseOf(name);
    const among = mannerOf(this.input.persona).autoPoses;
    // Another character: a pose she does not take goes at once.
    if (now >= this.autoAt || autoPoseNow(this.autoRest, among)) {
      this.autoRest = nextAutoPose(this.autoRest, Math.random(), among);
      this.autoAt = now + autoPosePause(Math.random());
    }
    return REST_POSES[this.autoRest];
  }

  /** The body of this frame: rest, emotion, state, slow life, movements, hands of speech. */
  private bodyNow(now: number, dt: number): RigPose {
    const { emotion, still, talking } = this.input;
    const state = bodyState(this.input.state ?? 'idle', this.input.working ?? false);
    this.wantIdle();
    const manner = mannerOf(this.input.persona);
    let target = mix({ ...ZERO_TRUNK, ...this.restNow(now) }, EMOTION_BODY[emotion], 1);
    if (state) target = mix(target, STATE_BODY[state], 1);
    // How her character holds her head, over whatever else turns it.
    target = add(target, { head: manner.carriage }, 1);
    this.body = mix(this.body, target, 1 - Math.exp(-dt / BODY_TAU_MS));
    let pose = this.body;
    if (still) return pose;
    // The idle animation moves on its own; without it, the slow life of the layers.
    pose = this.idle && !this.restOverride ? add(pose, motionOf(this.idleAt(now, this.idle.ms, this.idle.tracks), this.idle.first), 1) : add(pose, idleOffset(now, this.seed), manner.sway);
    // Thinking, her finger taps her chin.
    if (state === 'thinking') pose = add(pose, { rightIndexProximal: [0, 0, 16 * Math.max(0, Math.sin(now / 90))] }, 1);

    // She nods to a message the user sent.
    const sent = this.input.sentAt ?? -Infinity;
    if (sent > this.lastSent) {
      if (Number.isFinite(this.lastSent) || now - sent < 1500) this.moves.push({ tl: END_CUES.nod, start: now });
      this.lastSent = sent;
    }
    // Now and then an idle action, when nothing else goes on.
    if (now >= this.actionAt) {
      this.actionAt = now + idleActionPause(Math.random()) * manner.actionPause;
      const busy = talking || voice.playing || state !== null || this.moves.length > 0 || this.input.idleActions === false;
      if (!busy) {
        const names = manner.actions.length ? manner.actions : (Object.keys(IDLE_ACTIONS) as IdleAction[]);
        this.moves.push({
          tl: IDLE_ACTIONS[names[Math.floor(Math.random() * names.length)]],
          start: now,
        });
      }
    }
    this.speechMoves(now);
    for (const m of this.moves) {
      const s = sampleTimeline(m.tl, now - m.start);
      if (s) pose = mix(pose, s.pose, s.weight);
    }
    this.moves = this.moves.filter((m) => now - m.start <= m.tl.ms);
    if (this.beat) {
      const ms = now - this.beat.start;
      if (ms > 600) this.beat = null;
      else pose = add(pose, beatOffset(ms, this.beat.side, speechStrength(emotion) * manner.hands), 1);
    }
    return pose;
  }

  /** Beats of the phrase that sounds, and the nod or tilt when it ends. */
  private speechMoves(now: number) {
    const p = currentPhrase();
    const at = p?.startedAt ?? -1;
    if (at !== this.phraseAt) {
      // The phrase before is over: its last sign shows in the head.
      const cue = this.phraseText ? endCue(this.phraseText) : null;
      if (cue) this.moves.push({ tl: END_CUES[cue], start: now });
      this.phraseAt = at;
      this.phraseText = p?.text ?? '';
      this.beatTimes = p ? beats(p.levels, p.frameMs) : [];
      this.beatIndex = 0;
    }
    if (!p) return;
    const since = now - p.startedAt;
    while (this.beatIndex < this.beatTimes.length && this.beatTimes[this.beatIndex] <= since) {
      this.beatIndex += 1;
      this.beatSide = this.beatSide === 'left' ? 'right' : 'left';
      this.beat = { start: now, side: this.beatSide };
    }
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

  private aim(view: ModelFrame, aspect: number) {
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
    // A view grown by CSS zoom (the desktop window at a bigger size) gets as many pixels more.
    const zoom = (this.host as HTMLElement & { currentCSSZoom?: number }).currentCSSZoom ?? 1;
    this.renderer.setPixelRatio(Math.min((window.devicePixelRatio || 1) * zoom, MAX_PIXEL_RATIO));
    this.renderer.setSize(w, h, false);
    this.aim(this.input.frame ?? 'full', w / h);
  }

  attach(
    host: HTMLElement,
    hooks: {
      onLeave: (lastFrame: string) => void;
      onFail: (kind: FailKind) => void;
    },
  ) {
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
    // The preview looks inside her to check poses and lips.
    if (import.meta.env.DEV) (window as unknown as { mikaScene?: unknown }).mikaScene = this;
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
    const reframe = (input.frame ?? 'full') !== (this.input.frame ?? 'full');
    // A character with a face of her own wears it while nothing goes on.
    this.input = { ...input, emotion: restingEmotion(input.emotion, input.state ?? 'idle', mannerOf(input.persona)) };
    if (reframe) this.resize();
  }

  setProfile(profile: Profile) {
    this.profile = profile;
  }

  gesture(kind: Emotion | 'poke') {
    this.gestureKind = kind;
    this.gestureStart = performance.now();
  }

  touch(clientX: number, clientY: number): TouchPart | null {
    if (!this.host) return null;
    const box = this.host.getBoundingClientRect();
    const ndc = new THREE.Vector2(((clientX - box.left) / box.width) * 2 - 1, -((clientY - box.top) / box.height) * 2 + 1);
    const ray = new THREE.Raycaster();
    ray.setFromCamera(ndc, this.camera);
    const hit = ray.intersectObject(this.vrm.scene, true)[0];
    if (!hit) return null;
    const bones: Record<string, [number, number, number]> = {};
    for (const [name, node] of Object.entries(this.bones) as [RigBone, THREE.Object3D][]) {
      node.getWorldPosition(this.at);
      bones[name] = [this.at.x, this.at.y, this.at.z];
    }
    const bone = nearestBone([hit.point.x, hit.point.y, hit.point.z], bones);
    const head = bones.head;
    const headSize = Math.max(0.05, this.frame.topY - this.frame.headY);
    const up = head ? (hit.point.y - head[1]) / headSize : 0;
    return partOf(bone, up);
  }

  react(part: TouchPart, emotion?: Emotion) {
    // The face the click brought decides how her body answers (decision 0055).
    const tl = emotion ? reactionFor(part, emotion) : REACTION[part];
    if (tl && !this.input.still) this.moves.push({ tl, start: performance.now() });
  }

  probe(clientX: number, clientY: number, answer: (alpha: number) => void) {
    this.prober.ask(clientX, clientY, answer);
  }

  /** Answers an asked point with the opacity of the frame just drawn; nothing is read when nobody asked. */
  private readProbe() {
    const host = this.host;
    if (!host) return;
    this.prober.take((x, y) => {
      const box = host.getBoundingClientRect();
      const gl = this.renderer.getContext();
      const px = Math.floor(((x - box.left) / box.width) * gl.drawingBufferWidth);
      const py = Math.floor((1 - (y - box.top) / box.height) * gl.drawingBufferHeight);
      if (px < 0 || py < 0 || px >= gl.drawingBufferWidth || py >= gl.drawingBufferHeight) return 0;
      gl.readPixels(px, py, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, this.pixel);
      return this.pixel[3];
    });
  }

  perform(gesture: { name: string; motion: string | null }) {
    if (gesture.motion) {
      this.loadClip(gesture.motion)
        .then((clip) => this.startClip(clip))
        .catch(() => {
          // An animation that does not load is not played; the model stays.
        });
      return;
    }
    const tl = (GESTURES as Record<string, Timeline>)[gesture.name];
    if (tl) this.moves.push({ tl, start: performance.now() });
  }

  private async loadClip(id: string): Promise<THREE.AnimationClip> {
    const cached = this.clips.get(id);
    if (cached) return cached;
    const bytes = await invoke<ArrayBuffer>('avatar_motion_bytes', { id });
    const { loader, blocked } = guardedLoader('motion');
    const gltf = await loader.parseAsync(bytes, '');
    if (blocked()) throw new Error('the animation refers to outside files');
    const animation = (gltf.userData.vrmAnimations as VRMAnimation[] | undefined)?.[0];
    if (!animation) throw new Error('no animation in the file');
    // Only the bones: her face, lips and eyes stay with her own layers.
    const { rotation, translation } = createVRMAnimationHumanoidTracks(animation, this.vrm.humanoid, this.sign === -1 ? '0' : '1');
    const clip = new THREE.AnimationClip('motion', -1, [...rotation.values(), ...translation.values()]);
    this.clips.set(id, clip);
    return clip;
  }

  private startClip(clip: THREE.AnimationClip) {
    this.mixer ??= new THREE.AnimationMixer(this.vrm.scene);
    this.mixer.stopAllAction();
    const action = this.mixer.clipAction(clip);
    action.setLoop(THREE.LoopOnce, 1);
    action.clampWhenFinished = true;
    action.reset().play();
    const nodes = clip.tracks
      .map((track) => this.vrm.scene.getObjectByName(track.name.slice(0, track.name.lastIndexOf('.'))))
      .filter((node): node is THREE.Object3D => node !== undefined);
    const unique = [...new Set(nodes)];
    // The rest of every bone of the clip, once: the clip blends from it and gives it back.
    for (const node of unique) {
      if (!this.restOf.has(node)) this.restOf.set(node, { q: node.quaternion.clone(), p: node.position.clone() });
    }
    this.clip = { action, start: performance.now(), ms: clip.duration * 1000, nodes: unique };
  }

  /** Bones of the clip that no layer sets go back to rest; the hips stand where they stood. */
  private restClipBones(nodes: THREE.Object3D[]) {
    const own = new Set(Object.values(this.bones));
    for (const node of nodes) {
      const rest = this.restOf.get(node);
      if (!rest) continue;
      if (!own.has(node)) node.quaternion.copy(rest.q);
      if (node !== this.bones.hips) node.position.copy(rest.p);
    }
  }

  /** The animation of the user over the layers: in and out in 300 ms. */
  private applyClip(now: number) {
    const c = this.clip;
    if (!c || !this.mixer) return;
    const ms = now - c.start;
    // What the layers set this frame, or rest for bones they leave alone.
    this.restClipBones(c.nodes);
    if (ms > c.ms) {
      c.action.stop();
      this.clip = null;
      return;
    }
    const w = Math.min(1, ms / 300, (c.ms - ms) / 300);
    const kept = c.nodes.map((node) => [node.quaternion.x, node.quaternion.y, node.quaternion.z, node.quaternion.w, node.position.x, node.position.y, node.position.z]);
    c.action.time = ms / 1000;
    this.mixer.update(0);
    c.nodes.forEach((node, i) => {
      const k = kept[i];
      this.held.copy(node.quaternion);
      this.blendQ.set(k[0], k[1], k[2], k[3]);
      node.quaternion.slerpQuaternions(this.blendQ, this.held, w);
      this.blendP.set(k[4], k[5], k[6]);
      node.position.lerpVectors(this.blendP, node.position, w);
    });
  }

  push(vx: number, _vy: number) {
    this.drift.vx = vx;
  }

  private readonly tick = (now: number) => {
    this.raf = requestAnimationFrame(this.tick);
    if (
      !shouldRender({
        attached: this.host !== null,
        windowShown: this.input.windowShown,
        documentHidden: document.hidden,
      })
    ) {
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
      this.readProbe();
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
      if (now - this.blinkAt >= 190) this.blinkAt = now + (twiceBlink(Math.random()) ? 120 : blinkPause(Math.random()));
    }

    // Lips: by the sound of the voice while it plays (decision 0040), by
    // chance while a reply is written with the voice off.
    const gain = lipGain(this.profile, emotion);
    if (voice.playing) {
      const m = mouthNow(now);
      this.opening = smoothOpen(this.opening, m ? m.open : 0, dt);
      if (m?.shape) this.vowel = m.shape;
      for (const v of VOWELS) {
        this.lips[v] = approach(this.lips[v], this.vowel === v ? Math.min(1, gain * this.opening) : 0, dt, LIP_TAU_MS / 2);
      }
    } else {
      this.opening = 0;
      if (!talking) {
        this.vowel = null;
      } else if (now >= this.vowelAt) {
        this.vowel = nextVowel(this.vowel, Math.random());
        this.vowelAmp = 0.6 + 0.4 * Math.random();
        this.vowelAt = now + MOUTH_STEP_MS;
      }
      for (const v of VOWELS) {
        this.lips[v] = approach(this.lips[v], this.vowel === v ? Math.min(1, gain * this.vowelAmp) : 0, dt, LIP_TAU_MS);
      }
    }

    const out = new Map(this.emo);
    if (this.expressions.has('blink')) out.set('blink', Math.max(out.get('blink') ?? 0, blink));
    for (const v of VOWELS) if (this.expressions.has(v)) out.set(v, Math.max(out.get(v) ?? 0, this.lips[v]));
    this.applyExpressions(out);

    // What she looks at: the head and body turn to it, the eyes do the rest.
    const eyes = this.aimNow(now, dt, slot.gaze ?? null);

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
    // A window on the move: her top stays behind, then she rights herself in about 600 ms.
    const wantLean = windowLean(this.drift.vx);
    this.drift.lean = approach(this.drift.lean, wantLean, dt, 120);
    this.drift.vx = approach(this.drift.vx, 0, dt, 150);
    this.drift.shift = approach(this.drift.shift, -this.drift.vx / 20000, dt, 200);
    this.vrm.scene.position.x = this.drift.shift;
    const turned = add(
      this.bodyNow(now, dt),
      {
        head: [-this.turn.pitch.x, this.turn.yaw.x, 0],
        spine: [0, 0.6 * this.turn.body.x, this.drift.lean * 0.6],
        chest: [0, 0.4 * this.turn.body.x, this.drift.lean * 0.4],
      },
      1,
    );
    this.applyPose(this.pose, still ? 0 : breath(now), move, turned);
    this.applyClip(now);

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
    const look = eyes ?? slot.gaze ?? (this.wander && !still ? [this.wander.yaw, this.wander.pitch] : [0, 0]);
    glide(this.eyes.yaw, look[0], dt, EYES_GLIDE_MS, EYES_MAX_SPEED);
    glide(this.eyes.pitch, look[1], dt, EYES_GLIDE_MS, EYES_MAX_SPEED);
    // Eyes never stand quite still: small quick jumps, which do not glide.
    if (now >= this.jumpAt) {
      const j = saccade(Math.random(), Math.random());
      this.jump = still ? { yaw: 0, pitch: 0 } : j;
      this.jumpAt = now + j.ms;
    }
    const jumpTo = this.pursuing ? { yaw: 0, pitch: 0 } : this.jump;
    this.jumpNow.yaw = approach(this.jumpNow.yaw, jumpTo.yaw, dt, JUMP_TAU);
    this.jumpNow.pitch = approach(this.jumpNow.pitch, jumpTo.pitch, dt, JUMP_TAU);
    this.applyGaze(this.eyes.yaw.x + this.jumpNow.yaw, this.eyes.pitch.x + this.jumpNow.pitch);

    this.vrm.update(dt / 1000);
  }

  /**
   * Turns the head and body towards what she looks at and returns where the
   * eyes look past the head; null when the eyes keep their own way (the
   * gaze of the emotion or a glance aside).
   */
  private aimNow(now: number, dt: number, emotionGaze: [number, number] | null): [number, number] | null {
    const { still, cursor, typing } = this.input;
    const state = bodyState(this.input.state ?? 'idle', this.input.working ?? false);
    const typingNow = typing != null && now - typing.at < 1500;
    const cursorNow = this.input.follow !== false && !still && cursor != null && cursorInterest(cursor.movedAt, now);
    const source = pickGaze({
      typing: typingNow,
      state: state !== null,
      cursor: cursorNow,
      emotion: emotionGaze !== null,
    });
    let want = { headYaw: 0, headPitch: 0, eyeYaw: 0, eyePitch: 0, bodyYaw: 0 };
    let eyes: [number, number] | null = null;
    if ((source === 'typing' || source === 'cursor') && this.host) {
      const point = source === 'typing' ? typing! : cursor!;
      const a = angleTo(point, this.headOnScreen(), this.host.clientHeight * 1.5);
      want = aimAt(a.yaw, a.pitch);
      eyes = [want.eyeYaw, want.eyePitch];
    } else if (source === 'state' && state) {
      const [yaw, pitch] = STATE_GAZE[state];
      want = aimAt(yaw * 0.5, pitch * 0.5);
      eyes = [yaw * 0.5, pitch * 0.5];
    }
    this.pursuing = source === 'cursor' && now - cursor!.movedAt < PURSUIT_MS;
    // The head gets there in about 350 ms, the body later; both start and stop softly.
    glide(this.turn.yaw, want.headYaw, dt, HEAD_GLIDE_MS, HEAD_MAX_SPEED);
    glide(this.turn.pitch, want.headPitch, dt, HEAD_GLIDE_MS, HEAD_MAX_SPEED);
    glide(this.turn.body, want.bodyYaw, dt, BODY_GLIDE_MS, BODY_MAX_SPEED);
    return eyes;
  }

  /** Her eyes on the page, pixels of the window. */
  private headOnScreen(): { x: number; y: number } {
    const host = this.host!;
    const box = host.getBoundingClientRect();
    const head = this.bones.head;
    if (!head) return { x: box.left + box.width / 2, y: box.top + box.height * 0.2 };
    head.getWorldPosition(this.at);
    this.at.y += 0.06;
    this.at.project(this.camera);
    return {
      x: box.left + ((this.at.x + 1) / 2) * box.width,
      y: box.top + ((1 - this.at.y) / 2) * box.height,
    };
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

  private applyPose(pose: Pose, breathing: number, move: { y: number; roll: number } | null, body: RigPose = this.body) {
    const trunk = trunkPose(pose, breathing, move?.roll ?? 0, this.bones.chest !== undefined);
    // The layers add up; the head still stops where a person's would.
    this.applyRig(limitHead(add(body, trunk, 1)));
    const hips = this.bones.hips;
    // The hips stand where they stood: a clip that moved them gives them back.
    if (hips) hips.position.set(this.hipsRestX, this.hipsRest + (move?.y ?? 0) * this.height, this.hipsRestZ);
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

/** A model or animation file may only use what is inside it: any other address is refused. */
function guardedLoader(kind: 'model' | 'motion' = 'model'): {
  loader: GLTFLoader;
  blocked: () => boolean;
} {
  let blocked = false;
  const manager = new THREE.LoadingManager();
  manager.setURLModifier((url) => {
    if (/^(data|blob):/i.test(url)) return url;
    blocked = true;
    return 'data:,';
  });
  const loader = new GLTFLoader(manager);
  if (kind === 'model') loader.register((parser) => new VRMLoaderPlugin(parser));
  else loader.register((parser) => new VRMAnimationLoaderPlugin(parser));
  return { loader, blocked: () => blocked };
}

async function load(id: string, profile: Profile): Promise<LiveScene> {
  let renderer: THREE.WebGLRenderer;
  try {
    renderer = new THREE.WebGLRenderer({
      alpha: true,
      antialias: true,
      premultipliedAlpha: true,
    });
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
