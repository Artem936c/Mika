<script lang="ts">
  // A picture the assistant saved: generated or a screenshot. The file is read through
  // a command that serves only her own picture folders.
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { t } from './i18n';

  let { path }: { path: string } = $props();

  let src = $state('');
  let failed = $state(false);

  onMount(async () => {
    try {
      src = await invoke<string>('load_picture', { path });
    } catch {
      failed = true;
    }
  });
</script>

{#if src}
  <button class="pic" title={t('image.open')} aria-label={t('image.open')} onclick={() => invoke('open_picture', { path })}>
    <img {src} alt="" />
  </button>
{:else if failed}
  <p class="lost">{t('image.failed')}</p>
{:else}
  <div class="wait"></div>
{/if}

<style>
  .pic {
    all: unset;
    align-self: flex-start;
    cursor: pointer;
    border-radius: 16px;
    overflow: hidden;
    border: 1px solid var(--line);
    line-height: 0;
  }
  .pic:hover {
    border-color: rgba(143, 125, 255, 0.5);
  }
  img {
    display: block;
    max-width: 320px;
    max-height: 240px;
    object-fit: contain;
    background: rgba(255, 255, 255, 0.04);
  }
  .wait {
    align-self: flex-start;
    width: 200px;
    height: 120px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: linear-gradient(100deg, rgba(255, 255, 255, 0.03) 30%, rgba(255, 255, 255, 0.08) 50%, rgba(255, 255, 255, 0.03) 70%);
    background-size: 200% 100%;
    animation: slide 1.4s linear infinite;
  }
  @keyframes slide {
    to {
      background-position: -200% 0;
    }
  }
  .lost {
    margin: 0;
    font-size: 12px;
    color: var(--dim);
  }
  @media (prefers-reduced-motion: reduce) {
    .wait {
      animation: none;
    }
  }
</style>
