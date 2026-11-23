import { describe, expect, it } from 'vitest';
import { isMessageKey } from './i18n';
import { personaLabel } from './persona';

// The ids Rust sends in `PersonaView.ready` (`Persona::READY`).
const READY = ['caring', 'cheerful', 'calm', 'bold', 'business', 'sarcastic', 'sleepy'];

describe('personaLabel', () => {
  it('names every ready character from the interface texts', () => {
    for (const id of READY) {
      expect(isMessageKey(`persona.${id}`), id).toBe(true);
    }
    expect(personaLabel('business', READY)).toBe('Деловая');
    expect(personaLabel('sleepy', READY)).toBe('Сонная');
  });

  it('shows an own name as it is, even one that looks like a key', () => {
    expect(personaLabel('Котёнок', READY)).toBe('Котёнок');
    // An own character can never be called «own» or «adult»: only ready ids are looked up.
    expect(personaLabel('own', READY)).toBe('own');
    expect(personaLabel('adult', READY)).toBe('adult');
  });
});
