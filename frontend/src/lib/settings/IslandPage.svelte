<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';

  interface IslandSettings {
    city: string;
    hide_in_games: boolean;
    monitor: string;
    monitors: string[];
  }

  let s = $state<IslandSettings | null>(null);
  let city = $state('');
  let note = $state('');
  let error = $state('');

  onMount(async () => {
    try {
      s = await invoke<IslandSettings>('get_island_settings');
      city = s.city;
    } catch (e) {
      error = String(e);
    }
  });

  async function save(hide: boolean) {
    if (!s) return;
    try {
      await invoke('set_island_settings', { city, hideInGames: hide });
      s.hide_in_games = hide;
      s.city = city.trim();
      note = t('island.saved');
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  async function setMonitor(name: string) {
    if (!s) return;
    try {
      await invoke('set_island_monitor', { name });
      s.monitor = name;
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

{#if s}
  <div class="lbl">{t('island.city')}</div>
  <div class="line">
    <input class="fld" bind:value={city} maxlength="60" oninput={() => (note = '')} onkeydown={(e) => e.key === 'Enter' && save(s!.hide_in_games)} />
    <button class="bt" onclick={() => save(s!.hide_in_games)}>{t('tools.feed_save')}</button>
  </div>
  <p class="hint">{t('island.city_sub')}</p>
  {#if note}<p class="ok">{note}</p>{/if}

  <Switch checked={s.hide_in_games} title={t('island.games')} sub={t('island.games_sub')} onchange={save} />

  <div class="lbl mon">{t('island.monitor')}</div>
  <p class="hint">{t('island.monitor_sub')}</p>
  <div class="seg" role="radiogroup" aria-label={t('island.monitor')}>
    <button role="radio" aria-checked={s.monitor === ''} class:on={s.monitor === ''} onclick={() => setMonitor('')}>
      {t('island.monitor_primary')}
    </button>
    {#each s.monitors as m, i (m)}
      <button role="radio" aria-checked={s.monitor === m} class:on={s.monitor === m} title={m} onclick={() => setMonitor(m)}>
        {t('island.monitor_n', { n: i + 1 })}
      </button>
    {/each}
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .line {
    display: flex;
    gap: 8px;
  }
  .hint {
    margin: 8px 2px 14px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .mon {
    margin-top: 18px;
  }
  .ok {
    margin: -6px 2px 12px;
    font-size: 12.5px;
    color: var(--ok);
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
