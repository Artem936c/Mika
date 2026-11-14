<script lang="ts">
  // A video or sound tool found no FFmpeg: the offer to download it, made only on a press.
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { t } from './i18n';

  interface FfmpegStatus {
    found: boolean;
    downloading: boolean;
    download_mb: number;
  }

  let size = $state(161);
  let phase = $state<'offer' | 'loading' | 'done'>('offer');
  let share = $state(0);
  let error = $state('');

  onMount(() => {
    invoke<FfmpegStatus>('get_ffmpeg_status')
      .then((s) => {
        size = s.download_mb;
        if (phase === 'offer') phase = s.found ? 'done' : s.downloading ? 'loading' : 'offer';
      })
      .catch(() => {});
    const off = listen<number>('ffmpeg-progress', (e) => (share = e.payload));
    return () => {
      off.then((f) => f());
    };
  });

  async function get() {
    phase = 'loading';
    share = 0;
    error = '';
    try {
      const s = await invoke<FfmpegStatus>('install_ffmpeg');
      phase = s.found ? 'done' : 'offer';
    } catch (e) {
      error = String(e);
      phase = 'offer';
    }
  }
</script>

<div class="ff">
  <div class="head"><Icon name="alert" size={15} /><b>{t('chat.ffmpeg_title')}</b></div>
  {#if phase === 'done'}
    <p class="ok">{t('chat.ffmpeg_done')}</p>
  {:else}
    <p>{t('tools.ffmpeg_offer', { size })}</p>
    {#if phase === 'loading'}
      <div class="bar"><i style:width="{share}%"></i></div>
      <p class="dim">{t('chat.ffmpeg_loading', { share })}</p>
    {:else}
      <div class="acts"><button class="bt pri" onclick={get}>{t('chat.ffmpeg_get')}</button></div>
    {/if}
    <p class="dim">{t('chat.ffmpeg_or_path')}</p>
  {/if}
  {#if error}<p class="bad">{error}</p>{/if}
</div>

<style>
  .ff {
    align-self: flex-start;
    max-width: 96%;
    box-sizing: border-box;
    padding: 12px 14px;
    border-radius: 16px;
    border: 1px solid rgba(143, 125, 255, 0.35);
    background: rgba(143, 125, 255, 0.1);
    font-size: 13px;
    line-height: 1.45;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .head :global(.icon) {
    color: var(--v);
  }
  p {
    margin: 0 0 8px;
    user-select: text;
  }
  p:last-child {
    margin-bottom: 0;
  }
  .dim {
    font-size: 12px;
    color: var(--mute);
  }
  .ok {
    color: var(--ok);
  }
  .bad {
    font-size: 12px;
    color: var(--bad);
  }
  .acts {
    margin-bottom: 8px;
  }
  .bar {
    height: 4px;
    margin: 4px 0 8px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--p));
    transition: width 0.3s;
  }
</style>
