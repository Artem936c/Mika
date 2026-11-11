import { describe, expect, it } from 'vitest';
import { ISLAND_SHAPES, STATE_LABELS, islandRect, reduceState, shapeFor, unionRect } from './island';

describe('islandRect', () => {
  it('centres the island on the canvas', () => {
    expect(islandRect('idle', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
    expect(islandRect('awaiting_confirm', 1000)).toEqual({ x: 200, y: 14, w: 600, h: 268 });
  });
  it('falls back to idle for unknown state', () => {
    expect(islandRect('bogus', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
  });
});

describe('unionRect', () => {
  it('covers both rectangles', () => {
    expect(unionRect({ x: 382, y: 14, w: 236, h: 44 }, { x: 200, y: 14, w: 600, h: 268 })).toEqual({
      x: 200,
      y: 14,
      w: 600,
      h: 268,
    });
    expect(unionRect({ x: 0, y: 0, w: 10, h: 10 }, { x: 20, y: 5, w: 10, h: 10 })).toEqual({
      x: 0,
      y: 0,
      w: 30,
      h: 15,
    });
  });
});

describe('shapeFor', () => {
  it('returns the spec sizes', () => {
    expect(shapeFor('idle')).toEqual({ width: 236, height: 44, radius: 22 });
    expect(shapeFor('listening')).toEqual({ width: 340, height: 52, radius: 26 });
    expect(shapeFor('thinking')).toEqual({ width: 380, height: 52, radius: 26 });
    expect(shapeFor('speaking')).toEqual({ width: 620, height: 78, radius: 30 });
    expect(shapeFor('awaiting_confirm')).toEqual({ width: 600, height: 268, radius: 30 });
  });
  it('falls back to idle for unknown values', () => {
    expect(shapeFor('bogus')).toEqual(ISLAND_SHAPES.idle);
    expect(shapeFor(undefined)).toEqual(ISLAND_SHAPES.idle);
  });
});

describe('STATE_LABELS', () => {
  it('has a Russian label for every state', () => {
    expect(STATE_LABELS).toEqual({
      idle: 'Жду',
      listening: 'Слушаю',
      thinking: 'Думаю',
      speaking: 'Отвечаю',
      awaiting_confirm: 'Нужно подтверждение',
    });
  });
});

describe('reduceState', () => {
  it('follows state_changed', () => {
    expect(reduceState('idle', { type: 'state_changed', from: 'idle', to: 'listening' })).toBe('listening');
  });
  it('ignores other events', () => {
    expect(reduceState('thinking', { type: 'assistant_text', text: 'x' })).toBe('thinking');
  });
  it('ignores malformed payloads', () => {
    expect(reduceState('thinking', { type: 'state_changed', to: 'bogus' })).toBe('thinking');
    expect(reduceState('thinking', null)).toBe('thinking');
    expect(reduceState('thinking', 'state_changed')).toBe('thinking');
  });
});
