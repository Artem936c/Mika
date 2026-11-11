<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from './Switch.svelte';
  import { t } from './i18n';

  let { name }: { name: string } = $props();

  interface General {
    version: string;
    autostart: boolean;
    shortcut: string;
    data_dir: string;
    logs_dir: string;
  }

  let g = $state<General | null>(null);
  let error = $state('');
  let confirmWipe = $state(false);

  onMount(async () => {
    try {
      g = await invoke<General>('get_general_settings');
    } catch (e) {
      error = String(e);
    }
  });

  async function setAutostart(v: boolean) {
    try {
      await invoke('set_autostart', { value: v });
      if (g) g.autostart = v;
    } catch (e) {
      error = String(e);
    }
  }

  async function wipe() {
    if (!confirmWipe) {
      confirmWipe = true;
      return;
    }
    try {
      await invoke('wipe_all_data');
    } catch (e) {
      error = String(e);
      confirmWipe = false;
    }
  }
</script>

{#if g}
  <Switch
    checked={g.autostart}
    title={t('general.autostart')}
    sub={t('general.autostart_sub', { name })}
    onchange={setAutostart}
  />

  <div class="rows">
    <div class="row"><span>{t('general.shortcut')}</span><kbd>{g.shortcut}</kbd></div>
    <div class="row">
      <span>{t('general.data')}</span>
      <button class="link" title={g.data_dir} onclick={() => invoke('open_folder', { which: 'data' })}>{t('general.open')}</button>
    </div>
    <div class="row">
      <span>{t('general.log')}</span>
      <button class="link" title={g.logs_dir} onclick={() => invoke('open_folder', { which: 'logs' })}>{t('general.open')}</button>
    </div>
    <div class="row"><span>{t('general.version')}</span><span class="mute">{g.version}</span></div>
  </div>

  <div class="danger">
    <p>{t('general.wipe_text')}</p>
    <div class="acts">
      <button class="bt no" onclick={wipe}>{t(confirmWipe ? 'general.wipe_sure' : 'general.wipe')}</button>
      {#if confirmWipe}<button class="bt" onclick={() => (confirmWipe = false)}>{t('general.cancel')}</button>{/if}
    </div>
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .rows {
    margin: 12px 0;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 11px 2px;
    border-top: 1px solid var(--line);
    font-size: 14px;
  }
  .row:first-child {
    border-top: 0;
  }
  kbd {
    font: 600 12px var(--mono);
    padding: 3px 9px;
    border-radius: 8px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
  }
  .mute {
    color: var(--mute);
  }
  .link {
    all: unset;
    cursor: pointer;
    font-size: 13px;
    font-weight: 600;
    color: var(--v);
  }
  .link:hover {
    text-decoration: underline;
  }
  .danger {
    padding: 14px 16px;
    border-radius: 16px;
    border: 1px solid rgba(255, 107, 125, 0.28);
    background: rgba(255, 107, 125, 0.05);
  }
  .danger p {
    margin: 0 0 12px;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
  }
  .acts {
    display: flex;
    gap: 8px;
  }
  .err {
    margin: 10px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
