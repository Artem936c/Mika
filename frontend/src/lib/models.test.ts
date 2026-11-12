import { describe, expect, it } from 'vitest';
import { byPrice, contextLabel, money, searchModels, shortName, type ModelInfo } from './models';

const m = (id: string, price: number | null, extra: Partial<ModelInfo> = {}): ModelInfo => ({
  id,
  name: id,
  description: '',
  context: null,
  inputs: [],
  outputs: [],
  prices: price === null ? [] : [{ kind: 'prompt', value: price, per: '1M tokens' }],
  tools: false,
  ...extra,
});

describe('models', () => {
  it('context and money read well', () => {
    expect(contextLabel(131072)).toBe('128K');
    expect(contextLabel(2_097_152)).toBe('2M');
    expect(contextLabel(null)).toBe('');
    expect(money(2.68650525)).toBe('2,69');
    expect(money(0.00035784)).toBe('0,00036');
    expect(money(150)).toBe('150');
  });

  it('search matches every word in id, name or description', () => {
    const list = [m('openai/gpt-oss-20b', 2, { description: 'открытые веса' }), m('google/gemini-tts', 50)];
    expect(searchModels(list, 'oss').map((x) => x.id)).toEqual(['openai/gpt-oss-20b']);
    expect(searchModels(list, 'открытые 20b')).toHaveLength(1);
    expect(searchModels(list, '  ')).toHaveLength(2);
  });

  it('cheapest first, unknown prices last', () => {
    expect(byPrice([m('b', null), m('c', 5), m('a', 1)]).map((x) => x.id)).toEqual(['a', 'c', 'b']);
  });

  it('short name drops the provider prefix', () => {
    expect(shortName(m('x', 1, { name: 'OpenAI: gpt-oss-20b' }))).toBe('gpt-oss-20b');
    expect(shortName(m('plain', 1))).toBe('plain');
  });
});
