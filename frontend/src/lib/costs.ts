// Spending on providers and the summary of the day: the JSON the Rust side
// sends, and what the cards and the settings page decide from it. Pure
// functions. Money and units arrive as ready texts: nothing is priced or
// counted here.

export const COST_KINDS = ['chat', 'vision', 'speech', 'transcription', 'image', 'embedding'] as const;
export type CostKind = (typeof COST_KINDS)[number];

/** Money in one currency; `text` is ready to show. */
export interface Money {
  currency: string;
  cost: number;
  text: string;
}

/** Where the price of a model comes from. */
export type PriceSource = 'list' | 'manual' | 'none';

/** One line of a report: a kind, or a model of a kind. */
export interface UsageLine {
  kind: CostKind;
  /** Host of the provider; empty in a line of a kind. */
  provider: string;
  model: string;
  requests: number;
  /** Requests of unknown size: no usage, or a sound of unknown length. */
  blind: number;
  /** Requests with units and no price. */
  unpriced: number;
  tokens_in: number;
  tokens_cached: number;
  tokens_out: number;
  chars: number;
  seconds: number;
  images: number;
  /** The units as a ready text. */
  units: string;
  cost: Money[];
  free: boolean;
  price: PriceSource;
}

export interface CostReport {
  from: string;
  to: string;
  totals: Money[];
  kinds: UsageLine[];
  models: UsageLine[];
  requests: number;
  unpriced: number;
  blind: number;
  /** There are requests and every one cost nothing: a local model. */
  free: boolean;
}

/** Spending of today and of this month against the limits, in the main currency. */
export interface BudgetState {
  currency: string;
  day_spent: number;
  day_limit: number;
  /** Percent of the limit, 0 without one; may be above 100. */
  day_share: number;
  month_spent: number;
  month_limit: number;
  month_share: number;
  day_text: string;
  month_text: string;
}

/** A price the user typed for a model. A field of 0 is not set. */
export interface ManualPrice {
  provider: string;
  model: string;
  currency: string;
  input_mtok: number;
  output_mtok: number;
  per_kchar: number;
  per_minute: number;
  per_image: number;
}

/** What the settings page of spending shows. */
export interface CostsView {
  enabled: boolean;
  currency: string;
  /** False while the currency follows the address of the main provider. */
  currency_saved: boolean;
  /** The main provider does not tell its currency: its money is counted in the saved one. */
  currency_assumed: boolean;
  day_limit: number;
  month_limit: number;
  /** With a limit reached, paid work she starts by herself waits. */
  pause_unattended: boolean;
  /** The limit that holds such work right now. */
  paused: 'day' | 'month' | null;
  budget: BudgetState;
  today: CostReport;
  month: CostReport;
  manual: ManualPrice[];
  /** When the price lists were last read, `DD.MM HH:MM`. */
  prices_at: string | null;
}

/** The evening summary of the day in the settings. */
export interface EveningView {
  enabled: boolean;
  time: string;
}

/** Card of the tool `cost_report`. */
export interface CostReportData {
  kind: 'cost_report';
  label: string;
  totals: Money[];
  budget: BudgetState;
  kinds: { kind: string; name: string; money: string; units: string; requests: number; free: boolean }[];
  unpriced: number;
  blind: number;
  /** Every request cost nothing; absent in a card made before the field existed. */
  free: boolean;
}

/** Card of the tool `day_summary`. */
export interface DaySummaryData {
  kind: 'day_summary';
  date: string;
  title: string;
  /** Time at the computer; `null` when it is not counted. */
  app_total: string | null;
  apps: { name: string; seconds: number; text: string }[];
  counts: { id: string; value: string }[];
  spent: string;
}

export const CURRENCIES = [
  { code: 'RUB', sign: '₽' },
  { code: 'USD', sign: '$' },
  { code: 'EUR', sign: '€' },
] as const;

const PRICE_FIELDS = ['input_mtok', 'output_mtok', 'per_kchar', 'per_minute', 'per_image'] as const;
export type PriceField = (typeof PRICE_FIELDS)[number];

const num = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const str = (v: unknown): v is string => typeof v === 'string';

function obj(v: unknown): Record<string, unknown> | null {
  return typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

/** Every element read by `one`, or nothing when the list or one of them is broken. */
function all<T>(list: unknown, one: (v: unknown) => T | null): T[] | null {
  if (!Array.isArray(list)) return null;
  const out: T[] = [];
  for (const v of list) {
    const item = one(v);
    if (item === null) return null;
    out.push(item);
  }
  return out;
}

function money(v: unknown): Money | null {
  const m = obj(v);
  return m && str(m.currency) && num(m.cost) && str(m.text) ? { currency: m.currency, cost: m.cost, text: m.text } : null;
}

function budget(v: unknown): BudgetState | null {
  const b = obj(v);
  if (!b || !str(b.currency) || !str(b.day_text) || !str(b.month_text)) return null;
  if (!num(b.day_spent) || !num(b.day_limit) || !num(b.day_share)) return null;
  if (!num(b.month_spent) || !num(b.month_limit) || !num(b.month_share)) return null;
  return {
    currency: b.currency,
    day_spent: b.day_spent,
    day_limit: b.day_limit,
    day_share: b.day_share,
    month_spent: b.month_spent,
    month_limit: b.month_limit,
    month_share: b.month_share,
    day_text: b.day_text,
    month_text: b.month_text,
  };
}

function kindRow(v: unknown): CostReportData['kinds'][number] | null {
  const k = obj(v);
  if (!k || !str(k.kind) || !str(k.name) || !str(k.money) || !str(k.units) || !num(k.requests) || typeof k.free !== 'boolean') return null;
  return { kind: k.kind, name: k.name, money: k.money, units: k.units, requests: k.requests, free: k.free };
}

/** The card of spending, or nothing when the shape is not the one the chat draws. */
export function costCard(v: Record<string, unknown>): CostReportData | undefined {
  if (v.kind !== 'cost_report' || !str(v.label) || !num(v.unpriced) || !num(v.blind)) return undefined;
  const totals = all(v.totals, money);
  const limits = budget(v.budget);
  const kinds = all(v.kinds, kindRow);
  if (!totals || !limits || !kinds) return undefined;
  return { kind: 'cost_report', label: v.label, totals, budget: limits, kinds, unpriced: v.unpriced, blind: v.blind, free: v.free === true };
}

function appRow(v: unknown): DaySummaryData['apps'][number] | null {
  const a = obj(v);
  return a && str(a.name) && num(a.seconds) && a.seconds >= 0 && str(a.text) ? { name: a.name, seconds: a.seconds, text: a.text } : null;
}

function countRow(v: unknown): DaySummaryData['counts'][number] | null {
  const c = obj(v);
  return c && str(c.id) && str(c.value) ? { id: c.id, value: c.value } : null;
}

/** The card of the day, or nothing when the shape is not the one the chat draws. */
export function dayCard(v: Record<string, unknown>): DaySummaryData | undefined {
  if (v.kind !== 'day_summary' || !str(v.date) || !str(v.title) || !str(v.spent)) return undefined;
  if (v.app_total !== null && !str(v.app_total)) return undefined;
  const apps = all(v.apps, appRow);
  const counts = all(v.counts, countRow);
  if (!apps || !counts) return undefined;
  return { kind: 'day_summary', date: v.date, title: v.title, app_total: v.app_total, apps, counts, spent: v.spent };
}

/** Colour of a budget bar by the share of the limit spent, percent. */
export function barClass(share: number): '' | 'warn' | 'over' {
  if (share >= 100) return 'over';
  return share >= 80 ? 'warn' : '';
}

/** Width of a budget bar, percent of its track. */
export function barWidth(share: number): number {
  return Number.isFinite(share) ? Math.max(0, Math.min(100, share)) : 0;
}

/** Money of several currencies in one line, as the core wrote each. */
export function moneyLine(list: Money[]): string {
  return list.map((m) => m.text).join(' + ');
}

/**
 * What to say about a total: nothing was counted, every call was free, only
 * units are known, or money. Free is not «the price is unknown».
 */
export function spendState(report: { totals: Money[]; requests?: number; free?: boolean }): 'none' | 'free' | 'units' | 'money' {
  if (report.totals.length > 0) return 'money';
  if (report.free) return 'free';
  return (report.requests ?? 1) > 0 ? 'units' : 'none';
}

/** The text of a total that is not money; a key of the locale file. */
export function spendKey(state: 'none' | 'free' | 'units'): 'costs.all_free' | 'costs.units_only' | 'costs.none' {
  return state === 'free' ? 'costs.all_free' : state === 'units' ? 'costs.units_only' : 'costs.none';
}

/** Whether a report has money outside the budget: in another currency, or in an unknown one. */
export function foreignMoney(totals: Money[], main: string): boolean {
  return totals.some((m) => m.currency !== main);
}

/** The price fields a kind of call is billed by. */
export function priceFields(kind: string): PriceField[] {
  switch (kind) {
    case 'chat':
    case 'vision':
      return ['input_mtok', 'output_mtok'];
    case 'embedding':
      return ['input_mtok'];
    case 'speech':
      return ['per_kchar'];
    case 'transcription':
      return ['per_minute'];
    case 'image':
      return ['per_image'];
    default:
      return [];
  }
}

/**
 * The price to save for a model after the form of one `kind` was filled. The
 * saved price of that model is one entry for all its kinds: the fields the
 * form does not show keep their values. An empty field means «not set».
 */
export function mergedPrice(
  saved: ManualPrice[],
  provider: string,
  model: string,
  kind: string,
  typed: Partial<Record<PriceField, number | null | undefined>>,
  currency: string,
): ManualPrice {
  const old = saved.find((p) => p.provider === provider && p.model === model);
  const next: ManualPrice = {
    provider,
    model,
    currency,
    input_mtok: old?.input_mtok ?? 0,
    output_mtok: old?.output_mtok ?? 0,
    per_kchar: old?.per_kchar ?? 0,
    per_minute: old?.per_minute ?? 0,
    per_image: old?.per_image ?? 0,
  };
  for (const field of priceFields(kind)) {
    const value = typed[field];
    next[field] = num(value) ? value : 0;
  }
  return next;
}

/** The fields of a saved price that are set, with the number as the user typed it. */
export function priceParts(price: ManualPrice): { field: PriceField; value: string }[] {
  return PRICE_FIELDS.filter((field) => price[field] > 0).map((field) => ({ field, value: String(price[field]).replace('.', ',') }));
}
