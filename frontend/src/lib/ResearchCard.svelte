<script lang="ts">
  // The report of a deep search. Opening goes through the command that serves
  // only files the tools themselves wrote in this run of the app.
  import { invoke } from '@tauri-apps/api/core';
  import Icon from './Icon.svelte';
  import { clock, type ResearchReportData } from './chat';
  import { t } from './i18n';
  import { thousands } from './research';

  let { data }: { data: ResearchReportData } = $props();

  let error = $state('');

  async function open(reveal: boolean) {
    try {
      await invoke('open_av_file', { path: data.path, reveal });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="rs">
  <div class="file">
    <span class="ico"><Icon name="search" size={15} /></span>
    <div class="txt">
      <b title={data.path}>{data.title}</b>
      <small>
        {t('research.card_sources', { n: data.sources })}&nbsp;· {t('research.card_calls', { n: data.llm_calls })}&nbsp;·
        {t('research.card_tokens', { k: thousands(data.tokens) })}&nbsp;· {clock(data.seconds)}
      </small>
    </div>
    <div class="acts">
      <button class="bt" onclick={() => open(false)}>{t('chat.av_open')}</button>
      <button class="bt" onclick={() => open(true)}>{t('chat.av_reveal')}</button>
    </div>
  </div>
  {#each data.limits as limit (limit)}
    <p class="warn">{t(limit === 'time' ? 'research.card_limit_time' : 'research.card_limit_tokens')}</p>
  {/each}
  {#if data.unread > 0}<p class="note">{t('research.card_unread', { n: data.unread })}</p>{/if}
  {#if data.unsourced > 0}<p class="note">{t('research.card_unsourced', { n: data.unsourced })}</p>{/if}
  <p class="note">{t('research.card_note')}</p>
  {#if error}<p class="bad">{error}</p>{/if}
</div>

<style>
  .rs {
    align-self: flex-start;
    display: grid;
    gap: 4px;
    max-width: 96%;
    box-sizing: border-box;
    padding: 8px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.04);
  }
  .file {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    padding: 4px 4px 4px 6px;
  }
  .ico {
    flex: none;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 10px;
    color: var(--c);
    background: rgba(111, 227, 255, 0.1);
  }
  .txt {
    min-width: 0;
    flex: 1;
  }
  .txt b {
    display: block;
    font-size: 13px;
    font-weight: 700;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  small {
    display: block;
    margin-top: 2px;
    font-size: 11.5px;
    color: var(--mute);
    font-variant-numeric: tabular-nums;
  }
  .acts {
    flex: none;
    display: flex;
    gap: 6px;
  }
  .acts .bt {
    height: 30px;
    padding: 0 11px;
    font-size: 12px;
  }
  .note,
  .warn {
    margin: 0 6px;
    font-size: 11.5px;
    line-height: 1.4;
    color: var(--mute);
  }
  .warn {
    color: var(--warn);
  }
  .bad {
    margin: 2px 6px 2px;
    font-size: 12px;
    color: var(--bad);
    user-select: text;
  }
</style>
