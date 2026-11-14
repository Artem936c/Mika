<script lang="ts">
  // The evening summary of the day: on or off, its time, and «show now».
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import type { EveningView } from '../costs';
  import { t } from '../i18n';

  let v = $state<EveningView | null>(null);
  let time = $state('21:00');
  let note = $state('');
  let error = $state('');
  let busy = $state(false);

  onMount(async () => {
    try {
      v = await invoke<EveningView>('get_evening_summary');
      time = v.time;
    } catch (e) {
      error = String(e);
    }
  });

  async function save(enabled: boolean) {
    try {
      v = await invoke<EveningView>('set_evening_summary', { enabled, time });
      time = v.time;
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  // The command answers when the summary is in the chat: the model is asked on the way.
  async function showNow() {
    busy = true;
    error = '';
    note = t('day.evening_started');
    try {
      await invoke('evening_summary_now');
    } catch (e) {
      error = String(e);
    } finally {
      note = '';
      busy = false;
    }
  }
</script>

{#if v}
  <div class="evening">
    <Switch checked={v.enabled} title={t('day.evening_on')} sub={t('day.evening_sub')} onchange={save} />
    <div class="line">
      <span>{t('day.evening_time')}</span>
      <input class="fld" type="time" bind:value={time} onchange={() => v && save(v.enabled)} />
      <button class="bt" disabled={busy} onclick={showNow}>{t('day.evening_now')}</button>
    </div>
    {#if note}<p class="ok">{note}</p>{/if}
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .evening {
    margin-top: 10px;
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 10px 2px 4px;
    font-size: 13px;
  }
  .line .fld {
    width: 120px;
  }
  .ok {
    margin: 8px 2px 0;
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
