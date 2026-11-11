// Speech bubble of the desktop avatar. Pure functions.

import { tailWords } from './chat';
import type { MessageKey } from './i18n';
import type { AssistantState } from './island';

/** A fixed phrase from the locale file, or text that came from the assistant. */
export type Bubble = { key: MessageKey } | { text: string } | null;

/** Longest reply tail the bubble shows, characters. */
export const BUBBLE_CHARS = 140;

/**
 * What the bubble says. `confirm` is the summary of the action that waits for an answer.
 */
export function bubbleFor(state: AssistantState, reply: string, work: string, confirm: string | null): Bubble {
  if (confirm) return { text: confirm };
  switch (state) {
    case 'listening':
      return { key: 'bubble.listening' };
    case 'thinking':
      return work ? { text: work } : { key: 'bubble.thinking' };
    case 'speaking': {
      const text = tailWords(reply, BUBBLE_CHARS);
      return text ? { text } : null;
    }
    default:
      return null;
  }
}

/** How long a finished reply stays in the bubble: enough to read it, 4 to 12 seconds. */
export function lingerMs(text: string): number {
  return Math.min(12_000, Math.max(4_000, text.trim().length * 60));
}
