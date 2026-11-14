import { describe, expect, it } from 'vitest';
import { MINUTES, PAGES, SEARCHES, TOKENS_K, researchArgs, thousands, type ResearchView } from './research';

const saved: ResearchView = {
  folder: '',
  folder_shown: 'C:\\Users\\me\\Documents\\Mika\\Research',
  folder_default: 'C:\\Users\\me\\Documents\\Mika\\Research',
  model: '',
  model_shown: 'mistralai/mistral-nemo',
  max_searches: 5,
  max_pages: 8,
  max_minutes: 5,
  max_tokens: 50_000,
  llm_calls: 10,
  high_tokens: false,
  high_folder: false,
};

const form = { folder: '', model: '', searches: 5, pages: 8, minutes: 5, tokensK: 50 };

describe('researchArgs', () => {
  it('sends the form as the command takes it', () => {
    expect(researchArgs(saved, { folder: ' D:\\Reports ', model: ' cheap/model ', searches: 3, pages: 4, minutes: 6, tokensK: 120 })).toEqual({
      folder: 'D:\\Reports',
      model: 'cheap/model',
      maxSearches: 3,
      maxPages: 4,
      maxMinutes: 6,
      maxTokens: 120_000,
    });
  });

  it('keeps the saved value for an empty or broken number field', () => {
    expect(researchArgs(saved, { ...form, searches: null, pages: undefined, minutes: Number.NaN, tokensK: '70' })).toEqual({
      folder: '',
      model: '',
      maxSearches: 5,
      maxPages: 8,
      maxMinutes: 5,
      maxTokens: 50_000,
    });
  });

  it('keeps every limit inside its bounds and whole', () => {
    const low = researchArgs(saved, { ...form, searches: 0, pages: 1, minutes: -4, tokensK: 3 });
    expect([low.maxSearches, low.maxPages, low.maxMinutes, low.maxTokens]).toEqual([SEARCHES[0], PAGES[0], MINUTES[0], TOKENS_K[0] * 1000]);
    const high = researchArgs(saved, { ...form, searches: 99, pages: 99, minutes: 99, tokensK: 9999 });
    expect([high.maxSearches, high.maxPages, high.maxMinutes, high.maxTokens]).toEqual([SEARCHES[1], PAGES[1], MINUTES[1], TOKENS_K[1] * 1000]);
    const part = researchArgs(saved, { ...form, searches: 3.4, pages: 7.5, minutes: 2.2, tokensK: 49.6 });
    expect([part.maxSearches, part.maxPages, part.maxMinutes, part.maxTokens]).toEqual([3, 8, 2, 50_000]);
  });

  it('a saved limit that is not whole thousands still saves', () => {
    const odd = { ...saved, max_tokens: 50_400 };
    expect(researchArgs(odd, { ...form, tokensK: null }).maxTokens).toBe(50_000);
  });

  it('the bounds match the core', () => {
    expect([SEARCHES, PAGES, MINUTES, TOKENS_K]).toEqual([
      [2, 10],
      [2, 16],
      [2, 15],
      [20, 200],
    ]);
  });
});

describe('thousands', () => {
  it('rounds to whole thousands, never below one', () => {
    expect(thousands(38_200)).toBe('38');
    expect(thousands(38_500)).toBe('39');
    expect(thousands(150_000)).toBe('150');
    expect(thousands(400)).toBe('1');
    expect(thousands(0)).toBe('1');
    expect(thousands(Number.NaN)).toBe('1');
  });
});
