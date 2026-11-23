<script lang="ts">
  // Spending on providers under the tool row. Money and units come as ready
  // texts from the tool: the card only lays them out.
  import { barClass, barWidth, moneyLine, spendKey, spendState, type CostReportData } from './costs';
  import { t } from './i18n';

  let { data }: { data: CostReportData } = $props();

  const total = $derived(moneyLine(data.totals));
  // A card is drawn only when there were requests: no money is «free» or «units only».
  const state = $derived(spendState(data));
  // The limits are about today and this month, whatever period was asked.
  const limits = $derived(
    [
      { title: t('costs.today'), limit: data.budget.day_limit, share: data.budget.day_share, text: data.budget.day_text },
      { title: t('costs.month'), limit: data.budget.month_limit, share: data.budget.month_share, text: data.budget.month_text },
    ].filter((l) => l.limit > 0),
  );
</script>

<div class="cost">
  <div>
    <small>{t('costs.card_title', { label: data.label })}</small>
    <div class="sum" class:plain={state !== 'money'}>{state === 'money' ? total : t(spendKey(state))}</div>
  </div>
  {#each limits as l (l.title)}
    <div class="lim">
      <div class="cap"><span>{l.title}</span><span>{l.text}</span></div>
      <div class="bar {barClass(l.share)}"><i style:width="{barWidth(l.share)}%"></i></div>
    </div>
  {/each}
  {#if data.kinds.length}
    <div class="rows">
      {#each data.kinds as k, i (i)}
        <span class="name">{k.name}</span>
        <span class="units">{k.units}</span>
        <span class="money">{k.money || (k.free ? t('costs.free') : '')}</span>
      {/each}
    </div>
  {/if}
  {#if data.unpriced > 0 || data.blind > 0}
    <div class="notes">
      {#if data.unpriced > 0}<span>{t('costs.unpriced_n', { count: data.unpriced })}</span>{/if}
      {#if data.blind > 0}<span>{t('costs.blind_n', { count: data.blind })}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .cost {
    align-self: flex-start;
    display: grid;
    gap: 10px;
    width: 420px;
    max-width: 96%;
    box-sizing: border-box;
    padding: 12px 16px;
    border-radius: 18px;
    border: 1px solid rgba(95, 227, 161, 0.22);
    background: linear-gradient(135deg, rgba(95, 227, 161, 0.1), rgba(var(--c-rgb), 0.07));
    user-select: text;
  }
  small {
    display: block;
    font-size: 11.5px;
    color: var(--ink3);
  }
  .sum {
    margin-top: 2px;
    font-size: 26px;
    font-weight: 800;
    letter-spacing: -0.02em;
    line-height: 1.15;
    font-variant-numeric: tabular-nums;
  }
  .sum.plain {
    font-size: 14px;
    font-weight: 700;
    letter-spacing: 0;
  }
  .lim {
    display: grid;
    gap: 5px;
  }
  .cap {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    font-size: 11.5px;
    color: var(--mute);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--c));
  }
  .bar.warn i {
    background: var(--warn);
  }
  .bar.over i {
    background: var(--bad);
  }
  .rows {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: 5px 14px;
    font-size: 12.5px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .name::first-letter {
    text-transform: uppercase;
  }
  .units {
    color: var(--mute);
    white-space: nowrap;
  }
  .money {
    text-align: right;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .notes {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 14px;
    font-size: 11.5px;
    color: var(--dim);
  }
</style>
