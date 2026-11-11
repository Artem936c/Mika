<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

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
  <div class="general">
    <label class="switch">
      <input type="checkbox" checked={g.autostart} onchange={(e) => setAutostart(e.currentTarget.checked)} />
      <span><b>Запускать вместе с Windows</b>Мика появится островком после входа в систему.</span>
    </label>

    <div class="row"><span>Открыть чат с клавиатуры</span><kbd>{g.shortcut}</kbd></div>
    <div class="row">
      <span>Данные</span>
      <button class="link" title={g.data_dir} onclick={() => invoke('open_folder', { which: 'data' })}>Открыть папку</button>
    </div>
    <div class="row">
      <span>Журнал</span>
      <button class="link" title={g.logs_dir} onclick={() => invoke('open_folder', { which: 'logs' })}>Открыть папку</button>
    </div>
    <div class="row"><span>Версия</span><span class="mute">{g.version}</span></div>

    <div class="danger">
      <p>
        Удалить всё: ключи из диспетчера учётных данных, настройки, историю, заметки, правила, журнал и автозапуск. Мика
        закроется.
      </p>
      <button class="bt no" onclick={wipe}>{confirmWipe ? 'Точно удалить всё' : 'Удалить все данные'}</button>
      {#if confirmWipe}<button class="bt" onclick={() => (confirmWipe = false)}>Отмена</button>{/if}
    </div>
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .general {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .switch {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    font-size: 12.5px;
    color: var(--mute);
    line-height: 1.5;
  }
  .switch input {
    width: 18px;
    height: 18px;
    margin-top: 2px;
    accent-color: var(--v);
  }
  .switch b {
    display: block;
    color: var(--ink);
    font-size: 13.5px;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 13px;
    padding: 0 2px;
  }
  kbd {
    font: 12px 'Cascadia Code', Consolas, monospace;
    padding: 3px 8px;
    border-radius: 6px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
  }
  .link {
    all: unset;
    cursor: pointer;
    color: var(--c);
    font-weight: 600;
  }
  .mute {
    color: var(--mute);
  }
  .danger {
    margin-top: 6px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(255, 107, 125, 0.3);
    background: rgba(255, 107, 125, 0.05);
  }
  .danger p {
    margin: 0 0 10px;
    font-size: 12.5px;
    color: var(--mute);
  }
  .bt {
    height: 32px;
    padding: 0 14px;
    border-radius: 10px;
    font-size: 13px;
    font-weight: 700;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
    cursor: pointer;
  }
  .bt.no {
    color: var(--bad);
  }
  .err {
    color: var(--bad);
  }
</style>
