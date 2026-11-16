import { describe, expect, it } from 'vitest';
import { CLOSE_TAU_MS, OPEN_TAU_MS, flatTrack, lipTrack, mouthAt, smoothOpen, visemeOf } from './lips';

describe('visemeOf', () => {
  it('maps the vowels of both languages to the shapes of the format', () => {
    for (const ch of ['а', 'я', 'a', 'А']) expect(visemeOf(ch)).toBe('aa');
    for (const ch of ['о', 'o']) expect(visemeOf(ch)).toBe('oh');
    for (const ch of ['у', 'ю', 'u', 'w']) expect(visemeOf(ch)).toBe('ou');
    for (const ch of ['и', 'ы', 'i', 'y']) expect(visemeOf(ch)).toBe('ih');
    for (const ch of ['е', 'э', 'ё', 'e']) expect(visemeOf(ch)).toBe('ee');
  });

  it('closes the lips on m, b and p', () => {
    for (const ch of ['м', 'б', 'п', 'm', 'b', 'p', 'М']) expect(visemeOf(ch)).toBe('closed');
  });

  it('gives nothing for other letters, spaces and signs', () => {
    for (const ch of ['т', 'с', 'k', ' ', ',', '!', '7']) expect(visemeOf(ch)).toBe(null);
  });
});

describe('lipTrack', () => {
  it('lays the letters on the sounding steps only, in their order', () => {
    const track = lipTrack('аоу', [0, 255, 255, 255, 0], 20);
    expect(track).toHaveLength(5);
    expect(track[0].open).toBe(0);
    expect(track[4].open).toBe(0);
    expect(track.slice(1, 4).map((f) => f.shape)).toEqual(['aa', 'oh', 'ou']);
    expect(track[1].open).toBeCloseTo(1);
  });

  it('opens the mouth by loudness', () => {
    const track = lipTrack('ааа', [255, 128, 64], 20);
    expect(track[0].open).toBeGreaterThan(track[1].open);
    expect(track[1].open).toBeGreaterThan(track[2].open);
  });

  it('keeps the lips nearly shut on m, b and p', () => {
    const track = lipTrack('ама', [255, 255, 255], 20);
    expect(track[1].open).toBeLessThan(0.3);
  });

  it('opens on aa by loudness when the text has no letters', () => {
    const track = lipTrack('…', [0, 200], 20);
    expect(track[1].shape).toBe('aa');
    expect(track[1].open).toBeGreaterThan(0.5);
    expect(track[0].open).toBe(0);
  });
});

describe('flatTrack', () => {
  it('opens evenly on vowels and shuts on m, b and p', () => {
    const track = flatTrack('ам', 60, 20);
    expect(track).toHaveLength(6);
    expect(track[0].shape).toBe('aa');
    expect(track[0].open).toBeCloseTo(0.6);
    expect(track[4].open).toBeLessThan(0.3);
  });

  it('gives nothing for a text without letters', () => {
    expect(flatTrack('...', 60, 20)).toEqual([]);
  });
});

describe('mouthAt', () => {
  const track = lipTrack('а', [255, 255], 20);
  it('picks the step of the moment', () => {
    expect(mouthAt(track, 1000, 1025, 20)).toEqual(track[1]);
  });
  it('is null before the start and after the end', () => {
    expect(mouthAt(track, 1000, 990, 20)).toBe(null);
    expect(mouthAt(track, 1000, 1040, 20)).toBe(null);
  });
});

describe('smoothOpen', () => {
  it('opens faster than it closes', () => {
    expect(OPEN_TAU_MS).toBeLessThan(CLOSE_TAU_MS);
    const opened = smoothOpen(0, 1, 30);
    const closed = 1 - smoothOpen(1, 0, 30);
    expect(opened).toBeGreaterThan(closed);
  });
});
