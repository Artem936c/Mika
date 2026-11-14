import { describe, expect, it } from 'vitest';
import { cardData, reduceChat, toolLabel } from './chat';
import {
  COST_KINDS,
  barClass,
  barWidth,
  costCard,
  dayCard,
  mergedPrice,
  moneyLine,
  priceFields,
  priceParts,
  spendKey,
  spendState,
  foreignMoney,
  type ManualPrice,
} from './costs';
import { isMessageKey } from './i18n';
import { noticeOf } from './island';

/** The cards as the tools send them (contract of epic 16, section A8). */
const DAY = {
  kind: 'day_summary',
  date: '2026-11-14',
  title: 'пятница, 14 ноября',
  app_total: '6 ч 20 мин',
  apps: [
    { name: 'VS Code', seconds: 11100, text: '3 ч 05 мин' },
    { name: 'Chrome', seconds: 6000, text: '1 ч 40 мин' },
  ],
  counts: [
    { id: 'focus', value: '50 мин' },
    { id: 'meetings', value: '2' },
    { id: 'todos', value: '3' },
    { id: 'reminders', value: '2' },
    { id: 'tasks', value: '2' },
  ],
  spent: '12,40 ₽',
};

const COSTS = {
  kind: 'cost_report',
  label: 'сегодня',
  totals: [{ currency: 'RUB', cost: 12.4, text: '12,40 ₽' }],
  budget: {
    currency: 'RUB',
    day_spent: 12.4,
    day_limit: 100.0,
    day_share: 12,
    month_spent: 310.0,
    month_limit: 2000.0,
    month_share: 15,
    day_text: '12,40 ₽ из 100 ₽',
    month_text: '310 ₽ из 2 000 ₽',
  },
  kinds: [{ kind: 'chat', name: 'ответы модели', money: '9,10 ₽', units: '310 тыс. токенов', requests: 84, free: false }],
  unpriced: 3,
  blind: 2,
  free: false,
};

describe('the card of the day', () => {
  it('takes the summary as the tool sends it', () => {
    expect(dayCard(DAY)).toEqual(DAY);
    expect(cardData(DAY)).toEqual(DAY);
  });

  it('takes a day without counted time and without spending', () => {
    const quiet = { ...DAY, app_total: null, apps: [], counts: [], spent: '' };
    expect(dayCard(quiet)).toEqual(quiet);
  });

  it('keeps only what it draws', () => {
    expect(dayCard({ ...DAY, extra: 1, apps: [{ ...DAY.apps[0], title: 'private' }] })).toEqual({ ...DAY, apps: [DAY.apps[0]] });
  });

  it('refuses a broken shape', () => {
    expect(dayCard({ ...DAY, kind: 'day' })).toBeUndefined();
    expect(dayCard({ ...DAY, title: 14 })).toBeUndefined();
    expect(dayCard({ ...DAY, date: undefined })).toBeUndefined();
    expect(dayCard({ ...DAY, app_total: 380 })).toBeUndefined();
    expect(dayCard({ ...DAY, apps: 'VS Code' })).toBeUndefined();
    expect(dayCard({ ...DAY, apps: [{ name: 'VS Code', seconds: '11100', text: '3 ч' }] })).toBeUndefined();
    expect(dayCard({ ...DAY, apps: [{ name: 'VS Code', seconds: -1, text: '3 ч' }] })).toBeUndefined();
    expect(dayCard({ ...DAY, counts: [{ id: 'focus', value: 50 }] })).toBeUndefined();
    expect(dayCard({ ...DAY, counts: [null] })).toBeUndefined();
    expect(dayCard({ ...DAY, spent: null })).toBeUndefined();
    expect(cardData({ kind: 'day_summary' })).toBeUndefined();
  });

  it('has a label for every count the tool may send', () => {
    for (const id of ['focus', 'meetings', 'todos', 'reminders', 'tasks']) {
      expect(isMessageKey(`day.count.${id}`), id).toBe(true);
    }
  });
});

describe('the card of spending', () => {
  it('takes the report as the tool sends it', () => {
    expect(costCard(COSTS)).toEqual(COSTS);
    expect(cardData(COSTS)).toEqual(COSTS);
  });

  it('takes a report with units only', () => {
    const units = { ...COSTS, totals: [], kinds: [{ ...COSTS.kinds[0], money: '' }] };
    expect(costCard(units)).toEqual(units);
  });

  it('tells a free day from a price that is not known', () => {
    const free = { ...COSTS, totals: [], kinds: [{ ...COSTS.kinds[0], money: '', free: true }], unpriced: 0, blind: 1, free: true };
    expect(costCard(free)).toEqual(free);
    expect(spendState(costCard(free)!)).toBe('free');
    // A card without the field is not free: only units are known.
    const { free: _, ...old } = { ...COSTS, totals: [] };
    expect(costCard(old)?.free).toBe(false);
    expect(spendState(costCard(old)!)).toBe('units');
    expect(spendState(costCard(COSTS)!)).toBe('money');
  });

  it('refuses a broken shape', () => {
    expect(costCard({ ...COSTS, kind: 'costs' })).toBeUndefined();
    expect(costCard({ ...COSTS, label: 1 })).toBeUndefined();
    expect(costCard({ ...COSTS, totals: [{ currency: 'RUB', cost: '12.4', text: '12,40 ₽' }] })).toBeUndefined();
    expect(costCard({ ...COSTS, totals: [{ currency: 'RUB', cost: 12.4 }] })).toBeUndefined();
    expect(costCard({ ...COSTS, budget: null })).toBeUndefined();
    expect(costCard({ ...COSTS, budget: { ...COSTS.budget, day_share: '12' } })).toBeUndefined();
    expect(costCard({ ...COSTS, budget: { ...COSTS.budget, month_text: undefined } })).toBeUndefined();
    expect(costCard({ ...COSTS, kinds: {} })).toBeUndefined();
    expect(costCard({ ...COSTS, kinds: [{ ...COSTS.kinds[0], requests: '84' }] })).toBeUndefined();
    expect(costCard({ ...COSTS, kinds: [{ ...COSTS.kinds[0], free: 0 }] })).toBeUndefined();
    expect(costCard({ ...COSTS, unpriced: null })).toBeUndefined();
    expect(costCard({ ...COSTS, blind: Number.NaN })).toBeUndefined();
    expect(cardData({ kind: 'cost_report' })).toBeUndefined();
  });

  it('a failed tool shows no card', () => {
    const items = reduceChat([], { type: 'tool_finished', id: 'c1', name: 'cost_report', ok: false, output: 'нет', elapsed_ms: 3, data: COSTS });
    expect(items[0].data).toBeUndefined();
    const fine = reduceChat([], { type: 'tool_finished', id: 'c1', name: 'cost_report', ok: true, output: '', elapsed_ms: 3, data: COSTS });
    expect(fine[0].data).toEqual(COSTS);
  });
});

describe('budget bars', () => {
  it('turn amber at 80% and red at 100%', () => {
    expect(barClass(0)).toBe('');
    expect(barClass(79)).toBe('');
    expect(barClass(80)).toBe('warn');
    expect(barClass(99)).toBe('warn');
    expect(barClass(100)).toBe('over');
    expect(barClass(104)).toBe('over');
    expect(barClass(Number.NaN)).toBe('');
  });

  it('never leave their track', () => {
    expect(barWidth(12)).toBe(12);
    expect(barWidth(104)).toBe(100);
    expect(barWidth(-5)).toBe(0);
    expect(barWidth(Number.NaN)).toBe(0);
  });
});

describe('totals', () => {
  const rub = { currency: 'RUB', cost: 12.4, text: '12,40 ₽' };
  const usd = { currency: 'USD', cost: 0.42, text: '0,42 $' };

  it('are joined as the core wrote them', () => {
    expect(moneyLine([rub, usd])).toBe('12,40 ₽ + 0,42 $');
    expect(moneyLine([rub])).toBe('12,40 ₽');
    expect(moneyLine([])).toBe('');
  });

  it('tell nothing spent from a price that is not known', () => {
    expect(spendState({ totals: [], requests: 0 })).toBe('none');
    expect(spendState({ totals: [], requests: 6 })).toBe('units');
    expect(spendState({ totals: [rub], requests: 6 })).toBe('money');
  });

  it('tell a day of free calls from a price that is not known', () => {
    expect(spendState({ totals: [], requests: 6, free: true })).toBe('free');
    expect(spendState({ totals: [], requests: 6, free: false })).toBe('units');
    expect(spendKey('free')).toBe('costs.all_free');
    expect(spendKey('units')).toBe('costs.units_only');
    expect(spendKey('none')).toBe('costs.none');
    for (const key of ['costs.all_free', 'costs.units_only', 'costs.none', 'costs.currency_assumed', 'error.evening_failed']) {
      expect(isMessageKey(key), key).toBe(true);
    }
  });

  it('name money that is outside the budget', () => {
    expect(foreignMoney([rub], 'RUB')).toBe(false);
    expect(foreignMoney([], 'RUB')).toBe(false);
    expect(foreignMoney([rub, usd], 'RUB')).toBe(true);
    // A single bucket in a currency Mika does not know is outside the budget too.
    expect(foreignMoney([{ currency: '', cost: 3.2, text: '3,20' }], 'RUB')).toBe(true);
    expect(foreignMoney([{ currency: '', cost: 3.2, text: '3,20' }], '')).toBe(false);
  });
});

describe('manual prices', () => {
  it('ask for the units the kind is billed in', () => {
    expect(priceFields('chat')).toEqual(['input_mtok', 'output_mtok']);
    expect(priceFields('vision')).toEqual(['input_mtok', 'output_mtok']);
    expect(priceFields('embedding')).toEqual(['input_mtok']);
    expect(priceFields('speech')).toEqual(['per_kchar']);
    expect(priceFields('transcription')).toEqual(['per_minute']);
    expect(priceFields('image')).toEqual(['per_image']);
    expect(priceFields('video')).toEqual([]);
  });

  it('have a label for every field and every kind', () => {
    for (const kind of COST_KINDS) {
      expect(isMessageKey(`costs.kind.${kind}`), kind).toBe(true);
      for (const field of priceFields(kind)) expect(isMessageKey(`costs.price.${field}`), field).toBe(true);
    }
  });

  const saved: ManualPrice = {
    provider: 'api.openai.com',
    model: 'gpt-4o-mini-tts',
    currency: 'USD',
    input_mtok: 0.6,
    output_mtok: 12,
    per_kchar: 0,
    per_minute: 0,
    per_image: 0,
  };

  it('a price typed for one kind keeps the prices of the others', () => {
    // The same model answers in the chat and speaks: the speech form has one field.
    const next = mergedPrice([saved], 'api.openai.com', 'gpt-4o-mini-tts', 'speech', { per_kchar: 0.015 }, 'USD');
    expect(next).toEqual({ ...saved, per_kchar: 0.015 });
    // An empty or broken field is «not set».
    const cleared = mergedPrice([saved], 'api.openai.com', 'gpt-4o-mini-tts', 'chat', { input_mtok: null, output_mtok: Number.NaN }, 'EUR');
    expect(cleared).toEqual({ ...saved, currency: 'EUR', input_mtok: 0, output_mtok: 0 });
  });

  it('a new price starts from zeros and takes only the fields of its kind', () => {
    const next = mergedPrice([saved], 'api.openai.com', 'whisper-1', 'transcription', { per_minute: 0.006, per_image: 4 }, 'USD');
    expect(next).toEqual({
      provider: 'api.openai.com',
      model: 'whisper-1',
      currency: 'USD',
      input_mtok: 0,
      output_mtok: 0,
      per_kchar: 0,
      per_minute: 0.006,
      per_image: 0,
    });
  });

  it('are listed by the fields that are set', () => {
    expect(priceParts(saved)).toEqual([
      { field: 'input_mtok', value: '0,6' },
      { field: 'output_mtok', value: '12' },
    ]);
    expect(priceParts({ ...saved, input_mtok: 0, output_mtok: 0 })).toEqual([]);
  });
});

describe('the chat and the island', () => {
  it('the new tools have their labels', () => {
    expect(toolLabel('day_summary')).toBe('tool.day');
    expect(toolLabel('cost_report')).toBe('tool.costs');
    expect(isMessageKey('tool.day') && isMessageKey('tool.costs')).toBe(true);
  });

  it('the evening summary is a line of the chat, its head unfolds the island', () => {
    const text = 'Итог дня:\nСегодня ты шесть часов провёл за компьютером. Составить план на завтра?';
    const event = { type: 'remark', kind: 'day', text };
    expect(reduceChat([], event)).toEqual([{ kind: 'reminder', text }]);
    expect(noticeOf(event)).toBe('Итог дня.');
  });

  it('a budget warning is a line of the chat and unfolds the island', () => {
    const text = 'За сегодня потрачено 80,40 ₽ из 100 ₽: это 80% дневного лимита.';
    const event = { type: 'remark', kind: 'cost', text };
    expect(reduceChat([], event)).toEqual([{ kind: 'reminder', text }]);
    expect(noticeOf(event)).toBe(text);
  });
});
