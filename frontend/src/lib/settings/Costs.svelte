<script lang="ts">
  // Spending on paid providers: totals, the budget, prices. Every command
  // answers with the fresh view; money and units come as ready texts.
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import {
    CURRENCIES,
    barClass,
    barWidth,
    foreignMoney,
    mergedPrice,
    moneyLine,
    priceFields,
    priceParts,
    spendKey,
    spendState,
    type CostReport,
    type CostsView,
    type PriceField,
    type UsageLine,
  } from '../costs';
  import { t } from '../i18n';

  let v = $state<CostsView | null>(null);
  let error = $state('');
  let note = $state('');
  /** Which report the lists by kind and by model show. */
  let period = $state<'today' | 'month'>('today');
  // Number fields give `null` while they are empty.
  let dayLimit = $state<number | null>(0);
  let monthLimit = $state<number | null>(0);
  let currency = $state('RUB');
  /** The price lists are being read. */
  let reading = $state(false);
  /** The second click on the wipe button wipes. */
  let sure = $state(false);
  /** The model line whose price form is open. */
  let open = $state('');
  let typed = $state<Partial<Record<PriceField, number | null>>>({});
  let priceCurrency = $state('RUB');

  const report = $derived<CostReport | null>(v ? v[period] : null);
  // Money outside the budget: another currency, or one Mika does not know.
  const manyCurrencies = $derived(!!v && (foreignMoney(v.today.totals, v.currency) || foreignMoney(v.month.totals, v.currency)));

  /** A currency the selects can show; an address Mika does not know has none. */
  const known = (code: string) => (CURRENCIES.some((c) => c.code === code) ? code : 'RUB');
  const lineKey = (l: UsageLine) => `${l.kind}|${l.provider}|${l.model}`;

  /** `sync`: the budget fields take the saved values. Not after every command: the user may be typing. */
  function show(view: CostsView, sync: boolean) {
    v = view;
    if (!sync) return;
    dayLimit = view.day_limit;
    monthLimit = view.month_limit;
    currency = known(view.currency);
  }

  /** True when the command went through. */
  async function call(command: string, args: Record<string, unknown> = {}, sync = false): Promise<boolean> {
    try {
      show(await invoke<CostsView>(command, args), sync);
      error = '';
      return true;
    } catch (e) {
      error = String(e);
      return false;
    }
  }

  onMount(() => {
    call('get_costs', {}, true);
  });

  // Tauri takes command arguments in camelCase. An empty field is «no limit».
  async function saveBudget() {
    note = '';
    const saved = await call('set_cost_budget', { dayLimit: Number(dayLimit) || 0, monthLimit: Number(monthLimit) || 0, currency }, true);
    if (saved) note = t('island.saved');
  }

  function togglePrice(l: UsageLine) {
    if (!v) return;
    const key = lineKey(l);
    if (open === key) {
      open = '';
      return;
    }
    const saved = v.manual.find((p) => p.provider === l.provider && p.model === l.model);
    typed = Object.fromEntries(priceFields(l.kind).map((f) => [f, saved && saved[f] > 0 ? saved[f] : null]));
    priceCurrency = known(saved?.currency || v.currency);
    open = key;
  }

  async function savePrice(l: UsageLine) {
    if (!v) return;
    const price = mergedPrice(v.manual, l.provider, l.model, l.kind, typed, priceCurrency);
    if (await call('set_cost_price', { price })) open = '';
  }

  async function refresh() {
    reading = true;
    await call('refresh_cost_prices');
    reading = false;
  }

  async function clear() {
    if (!sure) {
      sure = true;
      return;
    }
    sure = false;
    await call('clear_costs');
  }
</script>

{#snippet tile(title: string, r: CostReport, limit: number, share: number, text: string)}
  {@const state = spendState(r)}
  <div class="tile">
    <small>{title}</small>
    {#if state === 'money'}
      <div class="sum">{moneyLine(r.totals)}</div>
    {:else}
      <div class="sum plain">{t(spendKey(state))}</div>
    {/if}
    {#if limit > 0}
      <div class="bar {barClass(share)}"><i style:width="{barWidth(share)}%"></i></div>
      <small class="of">{text}</small>
    {/if}
  </div>
{/snippet}

{#snippet currencies()}
  {#each CURRENCIES as c (c.code)}
    <option value={c.code}>{c.sign} {c.code}</option>
  {/each}
{/snippet}

{#if v}
  <Switch checked={v.enabled} title={t('costs.on')} sub={t('costs.on_sub')} onchange={(value) => call('set_costs_enabled', { value })} />

  <div class="tiles">
    {@render tile(t('costs.today'), v.today, v.budget.day_limit, v.budget.day_share, v.budget.day_text)}
    {@render tile(t('costs.month'), v.month, v.budget.month_limit, v.budget.month_share, v.budget.month_text)}
  </div>
  {#if manyCurrencies}<p class="hint">{t('costs.other_currency')}</p>{/if}

  <div class="seg pick" role="group">
    <button class:on={period === 'today'} aria-pressed={period === 'today'} onclick={() => (period = 'today')}>{t('costs.period_today')}</button>
    <button class:on={period === 'month'} aria-pressed={period === 'month'} onclick={() => (period = 'month')}>{t('costs.period_month')}</button>
  </div>

  {#if report}
    <div class="lbl">{t('costs.by_kind')}</div>
    {#if report.kinds.length === 0}
      <p class="hint">{t('costs.none')}</p>
    {:else}
      <div class="rows">
        {#each report.kinds as k (k.kind)}
          <div class="row">
            <b class="who">{t(`costs.kind.${k.kind}`)}</b>
            <span class="units">{k.units}</span>
            <span class="units">{t('costs.requests', { count: k.requests })}</span>
            <span class="money">{moneyLine(k.cost) || (k.free ? t('costs.free') : '')}</span>
          </div>
        {/each}
      </div>
    {/if}

    <div class="lbl">{t('costs.by_model')}</div>
    {#if report.models.length === 0}
      <p class="hint">{t('costs.none')}</p>
    {:else}
      <div class="rows">
        {#each report.models as m (lineKey(m))}
          <div class="model">
            <div class="row">
              <div class="who">
                <b title={m.model}>{m.model}</b>
                <small>{m.provider} · {t(`costs.kind.${m.kind}`)}</small>
              </div>
              <span class="units">{m.units}</span>
              <span class="money" class:mute={m.cost.length === 0}>{moneyLine(m.cost) || t(m.free ? 'costs.free' : 'costs.no_price')}</span>
            </div>
            <!-- A price of one's own can be corrected at the model it was set for. -->
            {#if m.unpriced > 0 || m.blind > 0 || m.price === 'manual'}
              <div class="notes">
                {#if m.price === 'manual'}<span class="tag">{t('costs.price_manual')}</span>{/if}
                {#if m.unpriced > 0}<span>{t('costs.unpriced_n', { count: m.unpriced })}</span>{/if}
                {#if m.blind > 0}<span>{t('costs.blind_n', { count: m.blind })}</span>{/if}
                {#if m.unpriced > 0 || m.price === 'manual'}
                  <button class="link" aria-expanded={open === lineKey(m)} onclick={() => togglePrice(m)}>{t('costs.price_set')}</button>
                {/if}
              </div>
            {/if}
            {#if open === lineKey(m)}
              <div class="form">
                {#each priceFields(m.kind) as f (f)}
                  <label>
                    <span>{t(`costs.price.${f}`)}</span>
                    <input class="fld" type="number" min="0" step="any" bind:value={typed[f]} onkeydown={(e) => e.key === 'Enter' && savePrice(m)} />
                  </label>
                {/each}
                <label>
                  <span>{t('costs.currency')}</span>
                  <select class="fld" bind:value={priceCurrency}>{@render currencies()}</select>
                </label>
                <button class="bt" onclick={() => savePrice(m)}>{t('costs.save')}</button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}

  <div class="lbl">{t('costs.budget')}</div>
  <div class="form">
    <label>
      <span>{t('costs.day_limit')}</span>
      <input class="fld" type="number" min="0" step="any" bind:value={dayLimit} oninput={() => (note = '')} />
    </label>
    <label>
      <span>{t('costs.month_limit')}</span>
      <input class="fld" type="number" min="0" step="any" bind:value={monthLimit} oninput={() => (note = '')} />
    </label>
    <label>
      <span>{t('costs.currency')}</span>
      <select class="fld" bind:value={currency} onchange={() => (note = '')}>{@render currencies()}</select>
    </label>
    <button class="bt" onclick={saveBudget}>{t('costs.save')}</button>
  </div>
  <p class="hint">{t('costs.budget_hint')}</p>
  {#if !v.currency_saved}<p class="hint">{t('costs.currency_auto')}</p>{/if}
  {#if v.currency_assumed}<p class="hint">{t('costs.currency_assumed')}</p>{/if}
  {#if note}<p class="ok">{note}</p>{/if}
  <div class="pause">
    <Switch
      checked={v.pause_unattended}
      title={t('costs.pause')}
      sub={t('costs.pause_sub')}
      onchange={(value) => call('set_cost_pause', { value })}
    />
    {#if v.paused}<p class="held">{t(v.paused === 'month' ? 'costs.paused_month' : 'costs.paused_day')}</p>{/if}
  </div>

  <div class="lbl">{t('costs.prices')}</div>
  <p class="hint">{t('costs.prices_hint')}</p>
  {#if v.manual.length}
    <div class="rows">
      {#each v.manual as p (`${p.provider}|${p.model}`)}
        <div class="row">
          <div class="who">
            <b title={p.model}>{p.model}</b>
            <small>{p.provider}</small>
          </div>
          <span class="units price">
            {#each priceParts(p) as part (part.field)}
              <span>{t(`costs.price.${part.field}`)}: {part.value} {p.currency}</span>
            {/each}
          </span>
          <button class="bt small" onclick={() => call('remove_cost_price', { provider: p.provider, model: p.model })}>{t('costs.price_remove')}</button>
        </div>
      {/each}
    </div>
  {/if}
  <div class="line">
    <span class="when">{v.prices_at ? t('costs.prices_at', { when: v.prices_at }) : t('costs.prices_never')}</span>
    <button class="bt" disabled={reading} onclick={refresh}>{t('costs.refresh')}</button>
  </div>

  <div class="line wipe">
    <button class="bt no" onclick={clear}>{t(sure ? 'costs.clear_sure' : 'costs.clear')}</button>
    {#if sure}<button class="bt" onclick={() => (sure = false)}>{t('general.cancel')}</button>{/if}
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .tiles {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    margin: 12px 0 10px;
  }
  .tile {
    min-width: 0;
    padding: 12px 16px 14px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    user-select: text;
  }
  small {
    display: block;
    font-size: 12px;
    color: var(--mute);
  }
  .sum {
    margin: 2px 0 0;
    font-size: 24px;
    font-weight: 800;
    letter-spacing: -0.02em;
    line-height: 1.2;
    font-variant-numeric: tabular-nums;
  }
  .sum.plain {
    padding: 6px 0 3px;
    font-size: 13.5px;
    font-weight: 600;
    letter-spacing: 0;
    color: var(--mute);
  }
  .bar {
    height: 4px;
    margin: 10px 0 6px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--c));
    transition: width 0.3s;
  }
  .bar.warn i {
    background: var(--warn);
  }
  .bar.over i {
    background: #ff6b7d;
  }
  .of {
    font-variant-numeric: tabular-nums;
  }
  .pick {
    margin-top: 6px;
  }
  .rows {
    display: grid;
    gap: 4px;
  }
  .model {
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.03);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 7px 10px;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.03);
    font-size: 13px;
    user-select: text;
  }
  .model .row {
    background: none;
  }
  .who {
    flex: 1;
    min-width: 0;
    font-weight: 600;
  }
  .who b {
    display: block;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who small {
    margin-top: 1px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .units {
    flex: none;
    font-size: 12.5px;
    color: var(--mute);
    white-space: nowrap;
  }
  .money {
    flex: none;
    min-width: 76px;
    text-align: right;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .money.mute {
    font-weight: 500;
    font-size: 12.5px;
    color: var(--dim);
  }
  .price {
    display: grid;
    gap: 1px;
    text-align: right;
    white-space: normal;
  }
  .notes {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
    padding: 0 10px 8px;
    font-size: 12px;
    color: var(--dim);
  }
  .tag {
    padding: 1px 8px;
    border-radius: 999px;
    color: var(--warn);
    background: rgba(255, 198, 92, 0.1);
  }
  .link {
    all: unset;
    cursor: pointer;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--v);
  }
  .link:hover {
    text-decoration: underline;
  }
  .link:focus-visible {
    outline: 2px solid rgba(143, 125, 255, 0.7);
    outline-offset: 2px;
  }
  .form {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px;
  }
  .model .form {
    padding: 2px 10px 10px;
  }
  .form label {
    display: grid;
    gap: 4px;
    font-size: 12px;
    color: var(--mute);
  }
  .form .fld {
    width: 150px;
    height: 36px;
  }
  .form select.fld {
    width: 96px;
  }
  select.fld {
    appearance: none;
    cursor: pointer;
  }
  select.fld option {
    background: #14151f;
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    margin: 10px 2px 0;
    font-size: 13px;
  }
  .when {
    flex: 1;
    min-width: 200px;
    color: var(--mute);
  }
  .wipe {
    margin-top: 22px;
  }
  .small {
    height: 30px;
    padding: 0 11px;
    font-size: 12px;
  }
  .hint {
    margin: 8px 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .ok {
    margin: 8px 2px 0;
    font-size: 12.5px;
    color: var(--ok);
  }
  .pause {
    margin-top: 12px;
  }
  .held {
    margin: 8px 2px 0;
    font-size: 12.5px;
    color: var(--warn);
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
