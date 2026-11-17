import { describe, expect, it } from 'vitest';
import { isMessageKey, t } from './i18n';
import { ruleLabel } from './rules';

describe('ruleLabel', () => {
  it('names the rule of system information commands', () => {
    const key = ruleLabel('class:system-info');
    expect(key).toBe('rules.class_system_info');
    expect(isMessageKey(key as string)).toBe(true);
    expect(t(key!)).not.toBe(key);
    // Letter case does not matter, as in the Rust side.
    expect(ruleLabel('CLASS:System-Info')).toBe('rules.class_system_info');
  });

  it('leaves other patterns as they are', () => {
    expect(ruleLabel('get-process')).toBeNull();
    expect(ruleLabel('class:unknown')).toBeNull();
    expect(ruleLabel('*')).toBeNull();
  });
});
