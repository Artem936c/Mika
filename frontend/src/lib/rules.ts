// Readable names of permanent rules. Pure functions.

import type { MessageKey } from './i18n';

/**
 * Patterns of class rules: one rule allows a whole class of calls (decision 0044).
 * Rust stores them as `class:<name>`; the letter case does not matter there either.
 */
const CLASS_LABELS: Record<string, MessageKey> = {
  'class:system-info': 'rules.class_system_info',
};

/** A readable name for a rule pattern, or `null` when the pattern is shown as it is. */
export function ruleLabel(pattern: string): MessageKey | null {
  return CLASS_LABELS[pattern.toLowerCase()] ?? null;
}
