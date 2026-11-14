// Fields of the "Deep search" settings and numbers of its chat card. Pure functions, no Tauri calls.

import { wholeMinutes } from './rhythm';

/** The deep search block as the core shows it. */
export interface ResearchView {
  /** The saved setting, may be empty. */
  folder: string;
  /** Where reports go now. */
  folder_shown: string;
  folder_default: string;
  /** The saved setting, may be empty. */
  model: string;
  /** The model the inner requests use now. */
  model_shown: string;
  max_searches: number;
  max_pages: number;
  max_minutes: number;
  max_tokens: number;
  /** Most paid model requests of one run. */
  llm_calls: number;
  high_tokens: boolean;
  high_folder: boolean;
}

/** Bounds of the limits, as the core keeps them. */
export const SEARCHES: [number, number] = [2, 10];
export const PAGES: [number, number] = [2, 16];
export const MINUTES: [number, number] = [2, 15];
/** Thousands of tokens. */
export const TOKENS_K: [number, number] = [20, 200];

/** What the form holds: number fields give `null` when empty. */
export interface ResearchForm {
  folder: string;
  model: string;
  searches: unknown;
  pages: unknown;
  minutes: unknown;
  tokensK: unknown;
}

/** Arguments of `set_research_settings`; an empty or broken number field keeps the saved value. */
export function researchArgs(saved: ResearchView, form: ResearchForm) {
  const tokensK = wholeMinutes(form.tokensK, TOKENS_K, Math.round(saved.max_tokens / 1000));
  return {
    folder: form.folder.trim(),
    model: form.model.trim(),
    maxSearches: wholeMinutes(form.searches, SEARCHES, saved.max_searches),
    maxPages: wholeMinutes(form.pages, PAGES, saved.max_pages),
    maxMinutes: wholeMinutes(form.minutes, MINUTES, saved.max_minutes),
    maxTokens: tokensK * 1000,
  };
}

/** Thousands of tokens for a card: `38`, at least `1`. */
export function thousands(tokens: number): string {
  const k = Number.isFinite(tokens) ? Math.round(tokens / 1000) : 0;
  return String(Math.max(1, k));
}
