// The provider's model list as the settings pages show it. Pure functions.

export interface Price {
  kind: string;
  value: number;
  per: string;
}

export interface ModelInfo {
  id: string;
  name: string;
  description: string;
  context: number | null;
  inputs: string[];
  outputs: string[];
  prices: Price[];
  tools: boolean;
}

export interface ModelList {
  models: ModelInfo[];
  currency: string;
  filtered: boolean;
}

export type Purpose = 'chat' | 'vision' | 'image' | 'embedding' | 'tts' | 'stt';

/** `131072` → `128K`, `1000000` → `1M`. */
export function contextLabel(n: number | null): string {
  if (!n) return '';
  if (n >= 1_000_000) return `${+(n / 1_048_576).toFixed(1)}M`.replace('.', ',');
  return `${Math.round(n / 1024)}K`;
}

/** `2,68`, `0,0004`, `50`: enough digits to tell cheap models apart. */
export function money(v: number): string {
  const digits = v >= 100 ? 0 : v >= 1 ? 2 : v >= 0.01 ? 3 : 5;
  return (+v.toFixed(digits)).toString().replace('.', ',');
}

/** Models whose id, name or description has every word of `query`. */
export function searchModels(list: ModelInfo[], query: string): ModelInfo[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return list;
  return list.filter((m) => {
    const hay = `${m.id} ${m.name} ${m.description}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** Cheapest first by the first price; models without a price go last. */
export function byPrice(list: ModelInfo[]): ModelInfo[] {
  const first = (m: ModelInfo) => (m.prices.length ? m.prices[0].value : Number.POSITIVE_INFINITY);
  return [...list].sort((a, b) => first(a) - first(b) || a.id.localeCompare(b.id));
}

/** Short name: the part after `Provider: `. */
export function shortName(m: ModelInfo): string {
  const i = m.name.indexOf(': ');
  return i > 0 ? m.name.slice(i + 2) : m.name;
}
