<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import LanguagePicker from './LanguagePicker.svelte';
  import Switch from './Switch.svelte';
  import Backup from './settings/Backup.svelte';
  import { t } from './i18n';

  let { name }: { name: string } = $props();

  interface General {
    version: string;
    autostart: boolean;
    shortcut: string;
    selection_shortcut: string;
    data_dir: string;
    logs_dir: string;
  }

  let g = $state<General | null>(null);
  let error = $state('');
  let confirmWipe = $state(false);
  let report = $state('');
  let copied = $state(false);

  interface UpdateInfo {
    current: string;
    manifest_url: string;
    check_at_start: boolean;
    available: { version: string; notes: string } | null;
  }
  let upd = $state<UpdateInfo | null>(null);
  let updNote = $state('');
  let updBusy = $state(false);
  let sureInstall = $state(false);

  async function loadUpdate() {
    upd = await invoke<UpdateInfo>('get_update_info').catch(() => null);
  }

  async function saveUpdate() {
    if (!upd) return;
    try {
      await invoke('save_update_settings', { manifestUrl: upd.manifest_url, checkAtStart: upd.check_at_start });
      updNote = '';
      return true;
    } catch (e) {
      updNote = String(e);
      return false;
    }
  }

  async function checkUpdate() {
    if (!upd || !(await saveUpdate())) return;
    updBusy = true;
    try {
      const found = await invoke<{ version: string; notes: string } | null>('check_update');
      upd.available = found;
      updNote = found ? '' : t('general.update_none');
    } catch (e) {
      updNote = String(e);
    } finally {
      updBusy = false;
    }
  }

  async function installUpdate() {
    if (!sureInstall) {
      sureInstall = true;
      return;
    }
    updBusy = true;
    updNote = t('general.update_downloading');
    try {
      await invoke('install_update');
    } catch (e) {
      updNote = String(e);
      sureInstall = false;
    } finally {
      updBusy = false;
    }
  }

  async function makeReport() {
    try {
      report = await invoke<string>('problem_report');
      copied = false;
    } catch (e) {
      error = String(e);
    }
  }

  async function copyReport() {
    try {
      await navigator.clipboard.writeText(report);
      copied = true;
    } catch (e) {
      error = String(e);
    }
  }

  onMount(async () => {
    try {
      g = await invoke<General>('get_general_settings');
      await loadUpdate();
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
  <LanguagePicker {name} />
  <Switch
    checked={g.autostart}
    title={t('general.autostart')}
    sub={t('general.autostart_sub', { name })}
    onchange={setAutostart}
  />

  <div class="rows">
    <div class="row"><span>{t('general.shortcut')}</span><kbd>{g.shortcut}</kbd></div>
    <div class="row"><span>{t('general.selection_shortcut')}</span><kbd>{g.selection_shortcut}</kbd></div>
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

  {#if upd}
    <div class="report">
      <p>{t('general.update_sub', { version: upd.current })}</p>
      <input class="fld" bind:value={upd.manifest_url} placeholder="https://…/mika-update.json" spellcheck="false" />
      <label class="chk">
        <input type="checkbox" bind:checked={upd.check_at_start} onchange={saveUpdate} />{t('general.update_at_start')}
      </label>
      <div class="acts">
        <button class="bt" onclick={checkUpdate} disabled={updBusy || !upd.manifest_url.trim()}>{t('general.update_check')}</button>
        {#if upd.available}
          <button class="bt pri" onclick={installUpdate} disabled={updBusy}>
            {t(sureInstall ? 'general.update_sure' : 'general.update_install', { version: upd.available.version })}
          </button>
        {/if}
      </div>
      {#if upd.available}
        {#if upd.available.notes}<pre>{upd.available.notes}</pre>{/if}
        <p class="warn">{t('general.update_unsigned')}</p>
      {/if}
      {#if updNote}<p class="note">{updNote}</p>{/if}
    </div>
  {/if}

  <div class="report">
    <p>{t('general.report_sub')}</p>
    <div class="acts">
      <button class="bt" onclick={makeReport}>{t('general.report')}</button>
      {#if report}<button class="bt" onclick={copyReport}>{t(copied ? 'general.report_copied' : 'general.report_copy')}</button>{/if}
    </div>
    {#if report}<pre>{report}</pre>{/if}
  </div>

  <Backup {name} />

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
  .report {
    padding: 14px 16px;
    margin-bottom: 12px;
    border-radius: 16px;
    border: 1px solid var(--line);
  }
  .report p {
    margin: 0 0 12px;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
  }
  .report pre {
    margin: 12px 0 0;
    max-height: 160px;
    overflow: auto;
    padding: 10px 12px;
    border-radius: 10px;
    background: rgba(0, 0, 0, 0.35);
    font: 11.5px/1.45 var(--mono);
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 10px 0 12px;
    font-size: 13px;
    color: var(--mute);
    cursor: pointer;
  }
  .chk input {
    accent-color: var(--v);
  }
  .warn {
    margin: 10px 0 0 !important;
    color: var(--warn) !important;
  }
  .note {
    margin: 10px 0 0 !important;
    user-select: text;
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
