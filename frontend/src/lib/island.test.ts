import { describe, expect, it } from 'vitest';
import {
  CHAT_NARROW,
  CHAT_WIDE,
  ISLAND_SHAPES,
  SETTINGS_SHAPE,
  islandRect,
  opensChat,
  reduceState,
  shapeFor,
  stateLabel,
  unionRect,
  viewShape,
} from './island';
import { isMessageKey } from './i18n';

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

describe('stateLabel', () => {
  it('has a text for every state', () => {
    for (const state of Object.keys(ISLAND_SHAPES)) {
      expect(isMessageKey(stateLabel(state as keyof typeof ISLAND_SHAPES)), state).toBe(true);
    }
  });
});

describe('viewShape', () => {
  it('sizes match the design', () => {
    expect(viewShape('chat', 'thinking', true)).toEqual({ width: 860, height: 500, radius: 34 });
    expect(viewShape('chat', 'thinking', false)).toEqual({ width: 600, height: 500, radius: 34 });
    expect(viewShape('settings', 'idle', false)).toEqual({ width: 860, height: 500, radius: 34 });
    expect([CHAT_WIDE, CHAT_NARROW, SETTINGS_SHAPE].every((s) => s.height === 500)).toBe(true);
  });
  it('island follows the assistant state', () => {
    expect(viewShape('island', 'speaking', true)).toEqual(ISLAND_SHAPES.speaking);
  });
  it('every shape fits the 1000 x 600 canvas below the top margin', () => {
    for (const s of [...Object.values(ISLAND_SHAPES), CHAT_WIDE, SETTINGS_SHAPE]) {
      expect(s.width).toBeLessThanOrEqual(1000 - 2 * 20);
      expect(14 + s.height).toBeLessThanOrEqual(600 - 20);
    }
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

describe('opensChat', () => {
  const at = (hit: boolean) => ({ closest: () => (hit ? {} : null) });

  it('opens on a click on the island body', () => {
    expect(opensChat(at(false))).toBe(true);
  });

  it('ignores clicks that started on a control inside the island', () => {
    expect(opensChat(at(true))).toBe(false);
  });

  it('treats a missing target as a click on the body', () => {
    expect(opensChat(null)).toBe(true);
  });
});
