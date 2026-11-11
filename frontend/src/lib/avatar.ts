// Built-in avatar: an SVG drawing with ten emotions. Pure functions, no DOM.
// A stand-in until the Live2D model of epic 7; the drawing is the one from the mockup.

export const EMOTION_ORDER = [
  'neutral',
  'joy',
  'shy',
  'surprise',
  'sad',
  'angry',
  'think',
  'wink',
  'sleepy',
  'worry',
] as const;

export type Emotion = (typeof EMOTION_ORDER)[number];

export function isEmotion(v: unknown): v is Emotion {
  return typeof v === 'string' && (EMOTION_ORDER as readonly string[]).includes(v);
}

/** The emotion after a bus event: only `emotion_changed` moves it. */
export function reduceEmotion(current: Emotion, event: unknown): Emotion {
  if (typeof event !== 'object' || event === null) return current;
  const e = event as { type?: unknown; emotion?: unknown };
  return e.type === 'emotion_changed' && isEmotion(e.emotion) ? e.emotion : current;
}

type Mouth = 'smile' | 'open' | 'o' | 'smallo' | 'frown' | 'shout' | 'wavy' | 'side' | 'cat';
type Effect = 'spark' | 'heart' | 'bang' | 'vein' | 'dots' | 'star' | 'zzz' | 'sweat';

/** How one emotion changes the face. Offsets are in drawing units (the face is 400 wide). */
export interface EmotionLook {
  /** `up`: the eye is a closed happy arc. */
  left?: 'up';
  right?: 'up';
  /** Upper eyelid, 0 (open) to 1 (shut). */
  lid?: number;
  /** Gaze offset. */
  gazeX?: number;
  gazeY?: number;
  /** Iris and whole-eye scale. */
  iris?: number;
  eye?: number;
  /** Inner and outer end of the brow, positive is down. */
  browIn?: number;
  browOut?: number;
  mouth: Mouth;
  /** Blush strength, 0 to 1. */
  blush: number;
  hatch?: boolean;
  tear?: boolean;
  effect?: Effect;
}

export const EMOTIONS: Record<Emotion, EmotionLook> = {
  neutral: { mouth: 'smile', blush: 0.3 },
  joy: { left: 'up', right: 'up', browIn: -4, browOut: -4, mouth: 'open', blush: 0.6, effect: 'spark' },
  shy: { lid: 0.22, gazeX: -4, gazeY: 5, browIn: -5, browOut: 4, mouth: 'wavy', blush: 1, hatch: true, effect: 'heart' },
  surprise: { iris: 0.74, eye: 1.08, browIn: -11, browOut: -10, mouth: 'o', blush: 0.18, effect: 'bang' },
  sad: { lid: 0.2, gazeY: 4, browIn: -8, browOut: 7, mouth: 'frown', blush: 0.12, tear: true },
  angry: { lid: 0.24, browIn: 10, browOut: -6, mouth: 'shout', blush: 0.5, effect: 'vein' },
  think: { gazeX: 5, gazeY: -6, browIn: -8, browOut: -6, mouth: 'side', blush: 0.2, effect: 'dots' },
  wink: { right: 'up', browIn: -3, browOut: -3, mouth: 'cat', blush: 0.45, effect: 'star' },
  sleepy: { lid: 0.55, gazeY: 5, browIn: 3, browOut: 5, mouth: 'smallo', blush: 0.2, effect: 'zzz' },
  worry: { iris: 0.88, browIn: -9, browOut: 6, mouth: 'wavy', blush: 0.1, effect: 'sweat' },
};

const LINE = 'stroke="#b8566e" stroke-width="2.4" fill="none" stroke-linecap="round" stroke-linejoin="round"';
const HAIR_EDGE = 'stroke="#8478c9" stroke-width="1.4" stroke-linejoin="round"';

const MOUTHS: Record<Mouth, string> = {
  smile: `<path d="M190 265q10 8 20 0" ${LINE}/>`,
  open: '<path d="M185 261q15 5 30 0q-1 21-15 21t-15-21Z" fill="#b83d5c" stroke="#8c2c46" stroke-width="1.5" stroke-linejoin="round"/><path d="M192 277q8-5 16 0q-3 5-8 5t-8-5Z" fill="#ff8fa8"/>',
  o: '<ellipse cx="200" cy="269" rx="5.5" ry="7.5" fill="#a53d55"/>',
  smallo: '<ellipse cx="200" cy="268" rx="3.5" ry="3" fill="#a53d55"/>',
  frown: `<path d="M191 271q9-8 18 0" ${LINE}/>`,
  shout: '<path d="M191 263h18l-9 12Z" fill="#a53d55" stroke="#8c2c46" stroke-width="1.5" stroke-linejoin="round"/>',
  wavy: `<path d="M188 268q4-5 8 0t8 0t8 0" ${LINE}/>`,
  side: `<path d="M197 268q6 2 11-2" ${LINE}/>`,
  cat: `<path d="M188 264q6 8 12 0q6 8 12 0" ${LINE}/>`,
};

const EFFECTS: Record<Effect, string> = {
  spark:
    '<path d="M318 80l4 12 12 4-12 4-4 12-4-12-12-4 12-4Z" fill="#ffd76a"/><path d="M86 92l3 8 8 3-8 3-3 8-3-8-8-3 8-3Z" fill="#ff9fd2"/>',
  heart: '<path d="M320 92c-8-12-24-2-14 12l14 14 14-14c10-14-6-24-14-12Z" fill="#ff7eb6"/>',
  bang: '<g stroke="#ffd76a" stroke-width="6" stroke-linecap="round"><path d="M316 72l-3 24M334 80l-9 22M311 110v.1M321 116v.1"/></g>',
  vein: '<g stroke="#ff5a6e" stroke-width="4.5" fill="none" stroke-linecap="round"><path d="M304 92q8 2 8-8M324 84q0 8 8 8M332 104q-8-2-8 8M312 112q0-8-8-8"/></g>',
  dots: '<g fill="#cfc8ff"><circle cx="306" cy="112" r="4"/><circle cx="320" cy="96" r="5.5"/><circle cx="338" cy="78" r="7.5"/></g>',
  star: '<path d="M300 132l5 13 14 1-11 9 4 14-12-8-12 8 4-14-11-9 14-1Z" fill="#ffd76a"/>',
  zzz: '<g fill="none" stroke="#cfc8ff" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"><path d="M300 102h15l-15 17h15"/><path d="M326 74h11l-11 13h11"/></g>',
  sweat: '<path d="M298 124c-10 15-10 27 0 27s10-12 0-27Z" fill="#9fe4ff" stroke="#5fb8e0" stroke-width="1.5"/>',
};

/** `side` is -1 for the left eye and 1 for the right one. */
function eye(cx: number, side: -1 | 1, look: EmotionLook, mode: 'up' | undefined): string {
  const cy = 214;
  const lid = look.lid ?? 0;
  const y = cy - 12 + lid * 30;
  const q = cy - 30 + lid * 46;
  if (mode === 'up') {
    return (
      `<path d="M${cx - 18} ${cy + 6}Q${cx} ${cy - 16} ${cx + 18} ${cy + 6}" stroke="#3a2748" stroke-width="5" fill="none" stroke-linecap="round"/>` +
      `<path d="M${cx + side * 18} ${cy + 6}l${side * 6} -4" stroke="#3a2748" stroke-width="3" stroke-linecap="round"/>`
    );
  }
  const clip = side < 0 ? 'mk-eye-l' : 'mk-eye-r';
  return (
    `<g class="eye" style="transform-origin:${cx}px ${cy + 12}px"><g transform="translate(${cx} ${cy}) scale(${look.eye ?? 1}) translate(${-cx} ${-cy})">` +
    `<g clip-path="url(#${clip})"><rect x="${cx - 22}" y="${cy - 24}" width="44" height="48" fill="#fff"/><ellipse cx="${cx}" cy="${cy - 21}" rx="24" ry="11" fill="#d6cfee"/>` +
    `<g transform="translate(${cx + (look.gazeX ?? 0)} ${cy + (look.gazeY ?? 0)}) scale(${look.iris ?? 1}) translate(${-cx} ${-cy})">` +
    `<ellipse cx="${cx}" cy="${cy + 2}" rx="15.5" ry="20" fill="url(#mk-iris)" stroke="#3a1450" stroke-width="1.6"/><ellipse cx="${cx}" cy="${cy - 9}" rx="15" ry="10" fill="#2a0f48" opacity=".45"/>` +
    `<ellipse cx="${cx}" cy="${cy + 3}" rx="6" ry="9" fill="#2a0f3e"/><ellipse cx="${cx}" cy="${cy + 15}" rx="9" ry="4.5" fill="#ffe3f1" opacity=".8"/>` +
    `<circle cx="${cx - 6}" cy="${cy - 9}" r="5.5" fill="#fff"/><circle cx="${cx + 7}" cy="${cy + 10}" r="2.6" fill="#fff"/></g>` +
    (lid ? `<path d="M${cx - 23} ${cy - 26}H${cx + 23}V${y}Q${cx} ${q} ${cx - 23} ${y}Z" fill="#f8d6cb"/>` : '') +
    `</g>` +
    `<path d="M${cx - side * 18} ${y}Q${cx + side} ${q} ${cx + side * 21} ${y + 3}" stroke="#3a2748" stroke-width="5.5" fill="none" stroke-linecap="round"/>` +
    `<path d="M${cx + side * 21} ${y + 3}l${side * 7} -6M${cx + side * 19} ${y + 8}l${side * 7} -2" stroke="#3a2748" stroke-width="2.8" stroke-linecap="round"/>` +
    `<path d="M${cx - side * 10} ${cy + 21}Q${cx + side * 3} ${cy + 25} ${cx + side * 15} ${cy + 18}" stroke="#8a5870" stroke-width="1.6" fill="none" stroke-linecap="round"/></g></g>`
  );
}

function brow(cx: number, side: -1 | 1, browIn: number, browOut: number): string {
  return `<path d="M${cx - side * 15} ${172 + browIn}Q${cx + side} ${164 + (browIn + browOut) / 2} ${cx + side * 18} ${173 + browOut}" stroke="#8d80cf" stroke-width="3.2" fill="none" stroke-linecap="round"/>`;
}

function bow(x: number, y: number): string {
  return `<g fill="#ff7eb6" stroke="#d94f93" stroke-width="1.2" stroke-linejoin="round"><path d="M${x} ${y}l-17-12q-5 12 0 24Z"/><path d="M${x} ${y}l17-12q5 12 0 24Z"/><circle cx="${x}" cy="${y}" r="5.5"/></g>`;
}

const VIEW_BOX = { head: '96 92 208 208', full: '0 0 400 520' } as const;

export type AvatarView = keyof typeof VIEW_BOX;

/**
 * SVG markup of the avatar. Gradients and clip paths come from `AvatarDefs.svelte`,
 * which must be on the page once. The string holds no outside data.
 */
export function avatarSvg(view: AvatarView, emotion: unknown): string {
  const look = EMOTIONS[isEmotion(emotion) ? emotion : 'neutral'];
  const browIn = look.browIn ?? 0;
  const browOut = look.browOut ?? 0;
  const blush = look.blush * 0.6;
  return `<svg class="girl" viewBox="${VIEW_BOX[view]}"><g class="all">
<g class="tl" style="transform-origin:128px 128px"><path d="M126 110C70 116 34 190 36 290c2 80 20 150 48 210 2-30 8-52 14-68 4 24 10 46 20 64-2-66 4-126 8-176 6-70 8-130 12-180Z" fill="url(#mk-tail)" ${HAIR_EDGE}/><path d="M70 190c-16 60-14 140 6 210" stroke="#fff" stroke-opacity=".6" stroke-width="5" fill="none" stroke-linecap="round"/><path d="M112 200c-6 80-8 150-2 220" stroke="#8f82d8" stroke-opacity=".5" stroke-width="2" fill="none"/></g>
<g class="tl tr" style="transform-origin:272px 128px"><path d="M274 110C330 116 366 190 364 290c-2 80-20 150-48 210-2-30-8-52-14-68-4 24-10 46-20 64 2-66-4-126-8-176-6-70-8-130-12-180Z" fill="url(#mk-tail)" ${HAIR_EDGE}/><path d="M330 190c16 60 14 140-6 210" stroke="#fff" stroke-opacity=".6" stroke-width="5" fill="none" stroke-linecap="round"/><path d="M288 200c6 80 8 150 2 220" stroke="#8f82d8" stroke-opacity=".5" stroke-width="2" fill="none"/></g>
<path d="M110 200C96 110 150 66 200 66s104 44 90 134c6 50 2 100-10 140H120c-12-40-16-90-10-140Z" fill="url(#mk-back)"/>
<path d="M184 276v44l-6 4 22 56 22-56-6-4v-44Z" fill="#ffe4d9"/><path d="M184 286q16 22 32 0v-10h-32Z" fill="#f1c0b5"/>
<path d="M178 320C152 332 124 346 112 372c-12 28-16 88-18 148h212c-2-60-6-120-18-148-12-26-40-40-66-52l-22 56Z" fill="#f8f6ff" stroke="#b9b3da" stroke-width="1.4"/>
<path d="M126 404q-6 60-4 116M274 404q6 60 4 116" stroke="#cfcae6" stroke-width="2" fill="none"/>
<path d="M184 316L120 354q8 38 26 60l54-28 54 28q18-22 26-60l-64-38-16 58Z" fill="url(#mk-collar)" stroke="#161a36" stroke-width="1.4" stroke-linejoin="round"/>
<path d="M131 360q8 28 20 44l49-26 49 26q12-16 20-44" stroke="#7fe3f5" stroke-width="2.4" fill="none" stroke-linejoin="round"/>
<g stroke="#d94f93" stroke-width="1.4" stroke-linejoin="round" fill="#ff7eb6"><path d="M200 390l-34-16q-6 18 2 36Z"/><path d="M200 390l34-16q6 18-2 36Z"/><path d="M196 398l-10 38 14-8 14 8-10-38Z"/><rect x="192" y="382" width="16" height="16" rx="5"/></g>
<g class="head">
<path d="M132 190C130 140 160 104 200 104s70 36 68 86c0 36-14 66-36 84-14 12-24 18-32 18s-18-6-32-18c-22-18-36-48-36-84Z" fill="url(#mk-skin)" stroke="#d59a8e" stroke-width="1.3"/>
<g clip-path="url(#mk-face)"><path d="M120 110H280V196l-10 4-12-20-14 28-14-30-10 20-12-22-8 38-10-38-12 22-10-20-14 30-12-28-12 20-10-4Z" fill="#f2beb4" opacity=".7"/></g>
<ellipse cx="158" cy="247" rx="15" ry="7" fill="#ff8fae" opacity="${blush}"/><ellipse cx="242" cy="247" rx="15" ry="7" fill="#ff8fae" opacity="${blush}"/>
${look.hatch ? '<path d="M148 242l-4 9M157 242l-4 9M166 242l-4 9M238 242l-4 9M247 242l-4 9M256 242l-4 9" stroke="#ff5f93" stroke-width="1.7" stroke-linecap="round"/>' : ''}
${eye(166, -1, look, look.left)}${eye(234, 1, look, look.right)}
${look.tear ? '<path d="M148 238c-6 9-6 16 0 16s6-7 0-16Z" fill="#9fe4ff" stroke="#5fb8e0" stroke-width="1.2"/>' : ''}
<path d="M200 244l-3 6" stroke="#dc9f92" stroke-width="1.8" stroke-linecap="round"/>
<g class="ms">${MOUTHS[look.mouth]}</g><ellipse class="mt" cx="200" cy="268" rx="6" ry="4.5" fill="#a53d55"/>
<path d="M128 168c-12 60-10 130 6 186 8-32 12-70 10-112 0-28-2-50-4-72Z" fill="url(#mk-hair)" ${HAIR_EDGE}/><path d="M272 168c12 60 10 130-6 186-8-32-12-70-10-112 0-28 2-50 4-72Z" fill="url(#mk-hair)" ${HAIR_EDGE}/>
<path d="M112 210C98 120 150 70 200 70s102 50 88 140Q286 180 278 160 278 178 270 190 264 168 256 152 254 180 244 198 238 170 230 150 228 174 220 188 214 164 208 148 206 184 200 204 196 170 190 148 188 172 178 188 174 164 168 150 164 180 154 198 148 172 142 154 138 176 130 190 124 174 120 162 114 182 112 210Z" fill="url(#mk-hair)" ${HAIR_EDGE}/>
<path d="M256 152q-6-32-20-56M230 150q-4-30-14-58M190 148q0-30 6-58M168 150q0-30 10-54M142 154q0-30 14-54" stroke="#9a8ede" stroke-width="1.5" fill="none" stroke-opacity=".8"/>
<path d="M136 128q14-20 30-6 18-18 34-2 16-16 34 2 16-14 30 6-14-10-30 4-18-16-34 0-16-16-34 0-16-14-30-4Z" fill="#fff" opacity=".8"/>
<path d="M200 72c-16-30 8-48 30-36-16 2-24 16-20 36Z" fill="url(#mk-hair)" ${HAIR_EDGE}/>
${bow(126, 122)}${bow(274, 122)}
<path d="M146 146l14 10M158 144l-10 14" stroke="#6fe3ff" stroke-width="4" stroke-linecap="round"/>
${brow(166, -1, browIn, browOut)}${brow(234, 1, browIn, browOut)}
${look.effect ? EFFECTS[look.effect] : ''}
</g></g></svg>`;
}
