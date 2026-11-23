<script lang="ts">
  import { weekdayShort, type WeatherData } from './chat';
  import { t } from './i18n';

  let { data }: { data: WeatherData } = $props();

  // Typographic minus, as in the mockup.
  const deg = (n: number) => `${n < 0 ? '−' : ''}${Math.abs(n)}°`;
  // The first day of the forecast is today; the card shows the next three.
  const days = $derived(data.days.slice(1, 4));
</script>

<div class="wx">
  <div>
    <div class="t">{deg(data.temp)}</div>
    <small>{data.city} · {data.text}</small>
    <small>{t('weather.feels', { feels: deg(data.feels).replace('°', ''), wind: data.wind })}</small>
  </div>
  {#if days.length}
    <div class="f">
      {#each days as d, i (d.date)}
        <div>
          {i === 0 ? t('weather.tomorrow') : weekdayShort(d.date)}
          <b>{deg(d.max)}</b>
          {d.text}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .wx {
    display: flex;
    align-items: center;
    gap: 18px;
    padding: 12px 16px;
    border-radius: 18px;
    max-width: 96%;
    box-sizing: border-box;
    border: 1px solid rgba(var(--c-rgb), 0.2);
    background: linear-gradient(135deg, rgba(60, 120, 200, 0.28), rgba(var(--v-rgb), 0.16));
  }
  .t {
    font-size: 38px;
    font-weight: 800;
    letter-spacing: -0.03em;
    line-height: 1;
  }
  small {
    display: block;
    font-size: 11.5px;
    color: var(--ink3);
    margin-top: 4px;
    white-space: nowrap;
  }
  .f {
    display: flex;
    gap: 8px;
    margin-left: auto;
  }
  .f div {
    padding: 6px 10px;
    border-radius: 12px;
    background: rgba(0, 0, 0, 0.25);
    font-size: 11px;
    text-align: center;
    color: var(--ink3);
    max-width: 78px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .f b {
    display: block;
    font-size: 14px;
    color: #fff;
  }
</style>
