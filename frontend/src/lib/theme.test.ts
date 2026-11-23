import { describe, expect, it } from 'vitest';
import {
  ACCENT_VARS,
  DARK_BACKGROUND,
  MAX_ACCENT_LUMINANCE,
  MIN_ACCENT_CONTRAST,
  PALETTES,
  THEMES,
  THEME_VARS,
  accentShades,
  apply,
  contrast,
  luminance,
  parseHex,
  themeChange,
  themeVars,
} from './theme';

const COLOUR = /^#[0-9a-f]{6}$|^rgba\(\d+, \d+, \d+, 0\.\d+\)$|^\d+, \d+, \d+$/;

describe('themes', () => {
  it('every theme sets every variable with a colour', () => {
    expect(Object.keys(PALETTES).sort()).toEqual([...THEMES].sort());
    for (const id of THEMES) {
      const p = PALETTES[id];
      expect(Object.keys(p).sort(), id).toEqual([...THEME_VARS].sort());
      for (const name of THEME_VARS) expect(p[name], `${id} ${name}`).toMatch(COLOUR);
    }
  });

  it('every theme is dark and readable', () => {
    for (const id of THEMES) {
      const p = PALETTES[id];
      expect(luminance(p['--panel']), id).toBeLessThan(0.02);
      expect(contrast(p['--ink'], p['--panel']), id).toBeGreaterThan(12);
      expect(contrast(p['--mute'], p['--panel']), id).toBeGreaterThanOrEqual(MIN_ACCENT_CONTRAST);
      expect(contrast(p['--v'], p['--panel']), id).toBeGreaterThanOrEqual(MIN_ACCENT_CONTRAST);
      // The text of a button sits on the accent; violet keeps its look of 2.9.
      expect(contrast(p['--on-v'], p['--v']), id).toBeGreaterThanOrEqual(2.8);
      expect(p['--v-rgb'], id).toBe(parseHex(p['--v'])!.join(', '));
    }
  });

  it('violet is the look of app.css', async () => {
    // Vitest hands CSS imports in empty, so the file is read as text. The
    // module name is built so the check of types does not look for Node types.
    const fs: { readFileSync(path: URL, encoding: string): string } = await import(/* @vite-ignore */ 'node' + ':fs');
    const css = fs.readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    const root = css.slice(css.indexOf(':root {'), css.indexOf('}', css.indexOf(':root {')));
    for (const name of THEME_VARS) {
      const m = new RegExp(`\\s${name}: ([^;]+);`).exec(root);
      expect(m?.[1], name).toBe(PALETTES.violet[name]);
    }
  });
});

describe('accentShades', () => {
  const readable = (hex: string) => {
    const s = accentShades(hex)!;
    expect(contrast(s.v, DARK_BACKGROUND), hex).toBeGreaterThanOrEqual(MIN_ACCENT_CONTRAST);
    expect(luminance(s.v), hex).toBeLessThanOrEqual(MAX_ACCENT_LUMINANCE);
    expect(contrast(s.vars['--on-v'], s.v), hex).toBeGreaterThanOrEqual(3);
    expect(Object.keys(s.vars).sort()).toEqual([...ACCENT_VARS].sort());
    return s;
  };

  it('lifts black to a readable colour and says so', () => {
    expect(readable('#000000').adjusted).toBe('dark');
    expect(readable('#1a1040').adjusted).toBe('dark');
  });

  it('brings white down from the text colour and says so', () => {
    const s = readable('#FFFFFF');
    expect(s.adjusted).toBe('light');
    expect(s.vars['--on-v']).not.toBe('#ffffff');
  });

  it('keeps a middle colour as it is', () => {
    const s = readable('#8f7dff');
    expect(s.adjusted).toBeNull();
    expect(s.v).toBe('#8f7dff');
    expect(s.vars['--v-rgb']).toBe('143, 125, 255');
    expect(s.vars['--on-v']).toBe('#ffffff');
    expect(readable('#40c0a0').adjusted).toBeNull();
  });

  it('takes nothing that is not #rrggbb', () => {
    expect(accentShades('pink')).toBeNull();
    expect(accentShades('#fff')).toBeNull();
  });
});

describe('themeVars and apply', () => {
  it('puts an own accent over the theme and keeps pink and cyan of the theme', () => {
    const vars = themeVars('mint', '#ff0080');
    expect(vars['--v']).not.toBe(PALETTES.mint['--v']);
    expect(vars['--p']).toBe(PALETTES.mint['--p']);
    expect(vars['--c']).toBe(PALETTES.mint['--c']);
    expect(vars['--panel']).toBe(PALETTES.mint['--panel']);
  });

  it('falls back to violet and to the colour of the theme', () => {
    expect(themeVars('light', '')).toEqual(PALETTES.violet);
    expect(themeVars('amber', 'nonsense')).toEqual(PALETTES.amber);
  });

  it('writes every variable on the root element', () => {
    const set = new Map<string, string>();
    const root = { style: { setProperty: (k: string, v: string) => set.set(k, v) }, dataset: {} as Record<string, string> };
    apply({ theme: 'sakura', accent: '' }, root as unknown as HTMLElement);
    expect(set.size).toBe(THEME_VARS.length);
    expect(set.get('--v')).toBe(PALETTES.sakura['--v']);
    expect(root.dataset.theme).toBe('sakura');
  });

  it('reads a change from the UI channel', () => {
    expect(themeChange({ action: 'theme_changed', theme: 'night', accent: '#123456' })).toEqual({ theme: 'night', accent: '#123456' });
    expect(themeChange({ action: 'theme_changed', theme: 'night' })).toEqual({ theme: 'night', accent: '' });
    expect(themeChange({ action: 'theme_changed', theme: 'light', accent: '' })).toBeNull();
    expect(themeChange({ action: 'language_changed', lang: 'en' })).toBeNull();
    expect(themeChange(null)).toBeNull();
  });
});
