import { t, isMessageKey } from './i18n';

/** An own character: a name to call it by and its text (`CustomPersona` in Rust). */
export interface CustomPersona {
  name: string;
  prompt: string;
}

/** The character part of the settings (`PersonaView` in Rust, decision 0051). */
export interface PersonaView {
  /** The character in force: the id of a ready one or the name of an own one. */
  choice: string;
  /** Ids of the ready characters, in their order. */
  ready: string[];
  custom: CustomPersona[];
  adult: boolean;
  max_custom: number;
  max_name: number;
  max_prompt: number;
}

/** The name shown for a character: the text of a ready one, the own name as it is. */
export function personaLabel(choice: string, ready: string[]): string {
  const key = `persona.${choice}`;
  return ready.includes(choice) && isMessageKey(key) ? t(key) : choice;
}
