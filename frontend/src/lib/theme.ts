// Themes of the windows (decision 0049). Rust keeps the choice; this file
// only turns it into CSS variables on the root element. Every theme is dark.

export const THEMES = ['violet', 'night', 'sakura', 'mint', 'amber', 'graphite'] as const;
export type ThemeId = (typeof THEMES)[number];

/** What Rust sends: `get_theme` and the `theme_changed` action of the UI channel. */
export interface ThemeView {
  theme: ThemeId;
  /** Own accent `#rrggbb`; empty is the colour of the theme. */
  accent: string;
}

/** The variables every theme sets. `app.css` holds the violet values as the default. */
export const THEME_VARS = [
  '--v',
  '--v-rgb',
  '--v-deep',
  '--v-mid',
  '--v-hi',
  '--on-v',
  '--ink-v',
  '--p',
  '--p-rgb',
  '--c',
  '--c-rgb',
  '--ink',
  '--ink2',
  '--ink3',
  '--mute',
  '--dim',
  '--island',
  '--glass',
  '--veil',
  '--panel',
  '--raised',
  '--raised-v',
  '--deep-a',
  '--deep-b',
  '--deep-c',
  '--halo-a',
  '--halo-b',
] as const;
export type ThemeVar = (typeof THEME_VARS)[number];
export type Palette = Record<ThemeVar, string>;

/** The variables an own accent replaces. */
export const ACCENT_VARS = ['--v', '--v-rgb', '--v-deep', '--v-mid', '--v-hi', '--on-v', '--ink-v'] as const;

type Base = Omit<Palette, '--v-rgb' | '--p-rgb' | '--c-rgb'>;

/** `--x-rgb` is the triplet of `--x`, for `rgba(var(--x-rgb), a)`. */
function withTriplets(base: Base): Palette {
  return {
    ...base,
    '--v-rgb': triplet(base['--v']),
    '--p-rgb': triplet(base['--p']),
    '--c-rgb': triplet(base['--c']),
  };
}

export const PALETTES: Record<ThemeId, Palette> = {
  // The look the app had from the start: the same values as app.css.
  violet: withTriplets({
    '--v': '#8f7dff',
    '--v-deep': '#6b58e6',
    '--v-mid': '#7663ee',
    '--v-hi': '#9d8dff',
    '--on-v': '#f3f4fa',
    '--ink-v': '#cdd3ff',
    '--p': '#ff7eb6',
    '--c': '#6fe3ff',
    '--ink': '#f3f4fa',
    '--ink2': '#c9cbe6',
    '--ink3': '#b9c3e6',
    '--mute': '#8d93ab',
    '--dim': '#5d6279',
    '--island': '#06070b',
    '--glass': 'rgba(16, 17, 27, 0.78)',
    '--veil': 'rgba(8, 9, 15, 0.66)',
    '--panel': '#0d0e17',
    '--raised': '#14151f',
    '--raised-v': '#14131f',
    '--deep-a': '#151331',
    '--deep-b': '#0b0b16',
    '--deep-c': '#191640',
    '--halo-a': '#4a4090',
    '--halo-b': '#1b1736',
  }),
  night: withTriplets({
    '--v': '#5f87ff',
    '--v-deep': '#3a60d6',
    '--v-mid': '#4a70e3',
    '--v-hi': '#7c9dff',
    '--on-v': '#ffffff',
    '--ink-v': '#cfdcff',
    '--p': '#b993ff',
    '--c': '#5fd4ff',
    '--ink': '#f1f5fc',
    '--ink2': '#c6cfe8',
    '--ink3': '#b4c5ea',
    '--mute': '#8996b5',
    '--dim': '#5a6684',
    '--island': '#05070c',
    '--glass': 'rgba(13, 17, 30, 0.78)',
    '--veil': 'rgba(7, 9, 16, 0.66)',
    '--panel': '#0b0e18',
    '--raised': '#121725',
    '--raised-v': '#111628',
    '--deep-a': '#111a36',
    '--deep-b': '#090c17',
    '--deep-c': '#142046',
    '--halo-a': '#34508f',
    '--halo-b': '#141c36',
  }),
  sakura: withTriplets({
    '--v': '#ec5a9b',
    '--v-deep': '#c93c7d',
    '--v-mid': '#db4a8c',
    '--v-hi': '#ff7fb5',
    '--on-v': '#ffffff',
    '--ink-v': '#ffd3e6',
    '--p': '#c9a2ff',
    '--c': '#8fd3ff',
    '--ink': '#fbf3f7',
    '--ink2': '#e6c9d8',
    '--ink3': '#e3bad0',
    '--mute': '#ab8d9f',
    '--dim': '#7a5d6f',
    '--island': '#0a0609',
    '--glass': 'rgba(24, 15, 23, 0.78)',
    '--veil': 'rgba(14, 8, 13, 0.66)',
    '--panel': '#120d14',
    '--raised': '#1b141d',
    '--raised-v': '#1d1320',
    '--deep-a': '#2a1426',
    '--deep-b': '#110a12',
    '--deep-c': '#33152e',
    '--halo-a': '#8a3f72',
    '--halo-b': '#2c1428',
  }),
  mint: withTriplets({
    '--v': '#2ecfa8',
    '--v-deep': '#16947a',
    '--v-mid': '#1ea888',
    '--v-hi': '#55e0bd',
    '--on-v': '#04120e',
    '--ink-v': '#c4f7e9',
    '--p': '#ff8fb3',
    '--c': '#7ae8ff',
    '--ink': '#f1faf8',
    '--ink2': '#c4e6df',
    '--ink3': '#b4e0d6',
    '--mute': '#88a8a2',
    '--dim': '#5a7a75',
    '--island': '#050a0a',
    '--glass': 'rgba(12, 21, 21, 0.78)',
    '--veil': 'rgba(6, 12, 12, 0.66)',
    '--panel': '#0a1112',
    '--raised': '#111a1b',
    '--raised-v': '#101c1b',
    '--deep-a': '#0f2a27',
    '--deep-b': '#081110',
    '--deep-c': '#11332e',
    '--halo-a': '#2f7f72',
    '--halo-b': '#12302b',
  }),
  amber: withTriplets({
    '--v': '#ffaa45',
    '--v-deep': '#d9822a',
    '--v-mid': '#e89434',
    '--v-hi': '#ffbe6b',
    '--on-v': '#1a0f02',
    '--ink-v': '#ffe4c2',
    '--p': '#ff8a7a',
    '--c': '#ffd98a',
    '--ink': '#fbf7ef',
    '--ink2': '#e6dbc4',
    '--ink3': '#e3d2b4',
    '--mute': '#ab9e8a',
    '--dim': '#7a6f5a',
    '--island': '#0a0805',
    '--glass': 'rgba(23, 19, 12, 0.78)',
    '--veil': 'rgba(13, 10, 6, 0.66)',
    '--panel': '#12100b',
    '--raised': '#1b1810',
    '--raised-v': '#1d170f',
    '--deep-a': '#2a1f10',
    '--deep-b': '#110d07',
    '--deep-c': '#33240f',
    '--halo-a': '#8a6a30',
    '--halo-b': '#2c2112',
  }),
  graphite: withTriplets({
    '--v': '#a9aebd',
    '--v-deep': '#7a7f8e',
    '--v-mid': '#878c9b',
    '--v-hi': '#c0c4d0',
    '--on-v': '#0c0c0e',
    '--ink-v': '#e2e3ea',
    '--p': '#c9a3b5',
    '--c': '#a6c6cf',
    '--ink': '#f4f4f6',
    '--ink2': '#d0d0d6',
    '--ink3': '#c4c4cc',
    '--mute': '#96969e',
    '--dim': '#66666e',
    '--island': '#070708',
    '--glass': 'rgba(18, 18, 20, 0.78)',
    '--veil': 'rgba(9, 9, 10, 0.66)',
    '--panel': '#0e0e10',
    '--raised': '#161618',
    '--raised-v': '#171719',
    '--deep-a': '#1c1c20',
    '--deep-b': '#0b0b0d',
    '--deep-c': '#222226',
    '--halo-a': '#55555e',
    '--halo-b': '#1e1e22',
  }),
};

export function isTheme(value: unknown): value is ThemeId {
  return typeof value === 'string' && (THEMES as readonly string[]).includes(value);
}

// ---- colour arithmetic ----

type Rgb = [number, number, number];

/** `#rrggbb` (hash optional) as numbers; `null` for anything else. */
export function parseHex(value: string): Rgb | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(value.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

export function toHex([r, g, b]: Rgb): string {
  const part = (x: number) => Math.round(Math.min(255, Math.max(0, x))).toString(16).padStart(2, '0');
  return `#${part(r)}${part(g)}${part(b)}`;
}

function triplet(hex: string): string {
  const rgb = parseHex(hex);
  return rgb ? rgb.join(', ') : '0, 0, 0';
}

/** Relative luminance, WCAG 2. */
export function luminance(hex: string): number {
  const rgb = parseHex(hex);
  if (!rgb) return 0;
  const [r, g, b] = rgb.map((x) => {
    const c = x / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** Contrast ratio of two colours, 1 to 21. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** `a` moved towards `b` by `t` (0 is `a`, 1 is `b`). */
function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
}

const WHITE: Rgb = [255, 255, 255];
const BLACK: Rgb = [0, 0, 0];
/** The darkest background an accent is drawn on (the settings window). */
export const DARK_BACKGROUND = '#0d0e17';
/** An accent reads on the dark background from this contrast up. */
export const MIN_ACCENT_CONTRAST = 4.5;
/** Above this luminance an accent is hard to tell from white text. */
export const MAX_ACCENT_LUMINANCE = 0.7;
/** White text sits on an accent from this contrast up, else dark text does. */
const WHITE_TEXT_CONTRAST = 3;
const DARK_TEXT = '#0b0b16';

export interface AccentShades {
  /** The accent to use: the given one, or lifted to a readable one. */
  v: string;
  vars: Record<(typeof ACCENT_VARS)[number], string>;
  /** Why the colour was changed: too dark or too light; `null` when it was not. */
  adjusted: 'dark' | 'light' | null;
}

/** The smallest step towards `to` that makes `ok` true; the colour is moved by at most all the way. */
function moveUntil(from: Rgb, to: Rgb, ok: (hex: string) => boolean): Rgb {
  for (let i = 1; i <= 40; i++) {
    const next = mix(from, to, i / 40);
    if (ok(toHex(next))) return next;
  }
  return to;
}

/** The accent variables of one colour: the shades of buttons, glows and borders. */
export function accentShades(hex: string): AccentShades | null {
  const rgb = parseHex(hex);
  if (!rgb) return null;
  let v = rgb;
  let adjusted: AccentShades['adjusted'] = null;
  if (contrast(toHex(v), DARK_BACKGROUND) < MIN_ACCENT_CONTRAST) {
    v = moveUntil(v, WHITE, (h) => contrast(h, DARK_BACKGROUND) >= MIN_ACCENT_CONTRAST);
    adjusted = 'dark';
  } else if (luminance(toHex(v)) > MAX_ACCENT_LUMINANCE) {
    v = moveUntil(v, BLACK, (h) => luminance(h) <= MAX_ACCENT_LUMINANCE);
    adjusted = 'light';
  }
  const main = toHex(v);
  const deep = toHex(mix(v, BLACK, 0.2));
  const on = contrast('#ffffff', main) >= WHITE_TEXT_CONTRAST ? '#ffffff' : DARK_TEXT;
  return {
    v: main,
    adjusted,
    vars: {
      '--v': main,
      '--v-rgb': v.map(Math.round).join(', '),
      '--v-deep': deep,
      '--v-mid': toHex(mix(v, BLACK, 0.12)),
      '--v-hi': toHex(mix(v, WHITE, 0.12)),
      '--on-v': on,
      '--ink-v': toHex(mix(v, WHITE, 0.7)),
    },
  };
}

/** Every variable of `theme` with the own `accent` over it; a bad accent is the theme's own. */
export function themeVars(theme: string, accent: string): Palette {
  const palette = PALETTES[isTheme(theme) ? theme : 'violet'];
  const shades = accent ? accentShades(accent) : null;
  return shades ? { ...palette, ...shades.vars } : { ...palette };
}

/** Writes the variables of `view` on the root element of this window. */
export function apply(view: Partial<ThemeView> | null | undefined, root: HTMLElement = document.documentElement): void {
  const vars = themeVars(view?.theme ?? 'violet', view?.accent ?? '');
  for (const name of THEME_VARS) root.style.setProperty(name, vars[name]);
  root.dataset.theme = isTheme(view?.theme) ? view.theme : 'violet';
}

/** A `theme_changed` action of the UI channel, or `null`. */
export function themeChange(payload: unknown): ThemeView | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const p = payload as Record<string, unknown>;
  if (p.action !== 'theme_changed' || !isTheme(p.theme)) return null;
  return { theme: p.theme, accent: typeof p.accent === 'string' ? p.accent : '' };
}
