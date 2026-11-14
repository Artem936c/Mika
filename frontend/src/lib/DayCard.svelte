<script lang="ts">
  // The summary of a day under the tool row. Every text comes ready from the
  // tool: the card only lays it out.
  import type { DaySummaryData } from './costs';
  import { isMessageKey, t } from './i18n';

  let { data }: { data: DaySummaryData } = $props();

  // A bar is the share of the most used program.
  const longest = $derived(Math.max(1, ...data.apps.map((a) => a.seconds)));

  const countLabel = (id: string) => {
    const key = `day.count.${id}`;
    return isMessageKey(key) ? t(key) : id;
  };
</script>

<div class="day">
  <b class="ttl">{data.title}</b>
  {#if data.app_total}
    <div class="total">{t('day.at_computer', { total: data.app_total })}</div>
    {#if data.apps.length}
      <div class="apps">
        {#each data.apps as a, i (i)}
          <span class="name" title={a.name}>{a.name}</span>
          <span class="track"><i style:width="{Math.min(100, (a.seconds / longest) * 100)}%"></i></span>
          <span class="time">{a.text}</span>
        {/each}
      </div>
    {/if}
  {/if}
  {#if data.counts.length}
    <div class="chips">
      {#each data.counts as c, i (i)}
        <span class="chip">{countLabel(c.id)}<b>{c.value}</b></span>
      {/each}
    </div>
  {/if}
  {#if data.spent}<div class="spent">{t('day.spent', { money: data.spent })}</div>{/if}
</div>

<style>
  .day {
    align-self: flex-start;
    display: grid;
    gap: 10px;
    width: 420px;
    max-width: 96%;
    box-sizing: border-box;
    padding: 12px 16px;
    border-radius: 18px;
    border: 1px solid rgba(143, 125, 255, 0.24);
    background: linear-gradient(135deg, rgba(143, 125, 255, 0.16), rgba(255, 126, 182, 0.08));
    user-select: text;
  }
  .ttl {
    display: block;
    font-size: 14px;
    font-weight: 800;
  }
  .ttl::first-letter {
    text-transform: uppercase;
  }
  .total {
    font-size: 12.5px;
    color: #c9cbe6;
  }
  .apps {
    display: grid;
    grid-template-columns: minmax(0, 120px) 1fr auto;
    align-items: center;
    gap: 6px 10px;
    font-size: 12px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .track {
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.1);
    overflow: hidden;
  }
  .track i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--p));
  }
  .time {
    color: var(--mute);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.25);
    font-size: 11.5px;
    color: #b9c3e6;
    white-space: nowrap;
  }
  .chip b {
    font-size: 12.5px;
    color: #fff;
    font-variant-numeric: tabular-nums;
  }
  .spent {
    font-size: 12px;
    color: var(--mute);
  }
</style>
