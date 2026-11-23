<script lang="ts">
  // Files a video or sound tool wrote. Opening goes through a command that serves
  // only files the tools themselves wrote in this run of the app.
  import { invoke } from '@tauri-apps/api/core';
  import Icon from './Icon.svelte';
  import { baseName, clock, megabytes, type AvFile } from './chat';
  import { t } from './i18n';

  let { files }: { files: AvFile[] } = $props();

  /** A whole folder gives up to a hundred files: the first ones are shown, the rest are counted. */
  const SHOWN = 6;

  let error = $state('');

  async function open(path: string, reveal: boolean) {
    try {
      await invoke('open_av_file', { path, reveal });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="av">
  {#each files.slice(0, SHOWN) as f (f.path)}
    <div class="file">
      <span class="ico"><Icon name="folder" size={15} /></span>
      <div class="txt">
        <b title={f.path}>{baseName(f.path)}</b>
        <small>
          {t('chat.av_mb', { value: megabytes(f.bytes) })}{#if f.duration_s !== undefined}&nbsp;· {clock(f.duration_s)}{/if}
        </small>
      </div>
      <div class="acts">
        <button class="bt" onclick={() => open(f.path, false)}>{t('chat.av_open')}</button>
        <button class="bt" onclick={() => open(f.path, true)}>{t('chat.av_reveal')}</button>
      </div>
    </div>
  {/each}
  {#if files.length > SHOWN}<p class="more">{t('chat.av_more', { n: files.length - SHOWN })}</p>{/if}
  {#if error}<p class="bad">{error}</p>{/if}
</div>

<style>
  .av {
    align-self: flex-start;
    display: grid;
    gap: 6px;
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
    background: rgba(var(--c-rgb), 0.1);
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
  .more {
    margin: 0 6px 2px;
    font-size: 12px;
    color: var(--mute);
  }
  .bad {
    margin: 2px 6px 2px;
    font-size: 12px;
    color: var(--bad);
    user-select: text;
  }
</style>
