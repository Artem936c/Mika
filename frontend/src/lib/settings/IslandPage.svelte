<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';
  import { PALETTES, THEMES, accentShades, apply as applyTheme, type ThemeId, type ThemeView } from '../theme';

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
  let look = $state<ThemeView | null>(null);
  /** The text of the colour field; it becomes the accent on Enter or on leaving the field. */
  let hex = $state('');
  /** The colour shown while the picker is dragged, before it is kept. */
  let trying = $state('');

  // The colour of the picker: the own accent, else the one of the theme.
  const shown = $derived(trying || look?.accent || (look ? PALETTES[look.theme]['--v'] : '#8f7dff'));
  // Why an own colour was lifted to a readable one.
  const adjusted = $derived(look && (trying || look.accent) ? (accentShades(trying || look.accent)?.adjusted ?? null) : null);

  onMount(async () => {
    try {
      s = await invoke<IslandSettings>('get_island_settings');
      city = s.city;
      look = await invoke<ThemeView>('get_theme');
      hex = look.accent;
    } catch (e) {
      error = String(e);
    }
  });

  async function setLook(theme: ThemeId, accent: string) {
    try {
      await invoke('set_theme', { theme, accent });
      look = await invoke<ThemeView>('get_theme');
      hex = look.accent;
      error = '';
    } catch (e) {
      error = String(e);
      // The colour tried in this window goes back to the kept one.
      if (look) applyTheme(look);
    }
    trying = '';
  }

  // Dragging the picker shows the colour in this window only; letting go keeps it.
  function tryAccent(value: string) {
    if (!look) return;
    trying = value;
    hex = value;
    applyTheme({ theme: look.theme, accent: value });
  }

  function keepHex() {
    if (!look || hex.trim() === look.accent) return;
    setLook(look.theme, hex.trim());
  }

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

{#if look}
  <div class="lbl">{t('island.look')}</div>
  <p class="hint top">{t('island.look_sub')}</p>
  <div class="themes" role="radiogroup" aria-label={t('island.look')}>
    {#each THEMES as id (id)}
      {@const p = PALETTES[id]}
      <button
        role="radio"
        aria-checked={look.theme === id}
        class="tile"
        class:on={look.theme === id}
        style="--tv: {p['--v']}; --tp: {p['--p']}; --tc: {p['--c']}; --tbg: {p['--panel']}; --tdeep: {p['--deep-a']}"
        onclick={() => setLook(id, '')}
      >
        <span class="sw"><i></i><i></i><i></i></span>
        <span class="nm">{t(`theme.${id}`)}</span>
      </button>
    {/each}
  </div>

  <div class="lbl acc">{t('theme.accent')}</div>
  <div class="line">
    <input
      class="pick"
      type="color"
      value={shown}
      aria-label={t('theme.accent')}
      oninput={(e) => tryAccent(e.currentTarget.value)}
      onchange={(e) => setLook(look!.theme, e.currentTarget.value)}
    />
    <input class="fld hex" bind:value={hex} maxlength="7" placeholder={PALETTES[look.theme]['--v']} spellcheck="false" onkeydown={(e) => e.key === 'Enter' && keepHex()} onblur={keepHex} />
    {#if look.accent}
      <button class="bt" onclick={() => setLook(look!.theme, '')}>{t('theme.accent_reset')}</button>
    {/if}
  </div>
  <p class="hint">{t('theme.accent_sub')}</p>
  {#if adjusted}<p class="warnline">{t(adjusted === 'dark' ? 'theme.accent_dark' : 'theme.accent_light')}</p>{/if}
{/if}

{#if s}
  <div class="lbl" class:sec={look}>{t('island.city')}</div>
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
  .mon,
  .acc,
  .sec {
    margin-top: 18px;
  }
  .hint.top {
    margin-top: 0;
  }
  .themes {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .tile {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: linear-gradient(160deg, var(--tdeep), var(--tbg));
    transition:
      border-color 0.2s,
      box-shadow 0.2s;
  }
  .tile:hover {
    border-color: var(--line2);
  }
  .tile.on {
    border-color: var(--tv);
    box-shadow: 0 0 0 1px var(--tv) inset;
  }
  .tile:focus-visible {
    outline: 2px solid var(--tv);
    outline-offset: 1px;
  }
  .sw {
    display: flex;
    flex: none;
  }
  .sw i {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--tbg);
    margin-left: -5px;
    background: var(--tv);
  }
  .sw i:first-child {
    margin-left: 0;
  }
  .sw i:nth-child(2) {
    background: var(--tp);
  }
  .sw i:nth-child(3) {
    background: var(--tc);
  }
  .nm {
    font-size: 12.5px;
    font-weight: 700;
    color: var(--ink);
    white-space: nowrap;
  }
  .pick {
    flex: none;
    box-sizing: border-box;
    width: 44px;
    height: 40px;
    padding: 4px;
    border-radius: 12px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.04);
    cursor: pointer;
  }
  .pick::-webkit-color-swatch-wrapper {
    padding: 0;
  }
  .pick::-webkit-color-swatch {
    border: 0;
    border-radius: 8px;
  }
  .hex {
    width: 120px;
    flex: none;
    font-family: var(--mono);
  }
  .warnline {
    margin: -6px 2px 12px;
    font-size: 12.5px;
    color: var(--warn);
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
