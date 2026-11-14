<script lang="ts">
  // The interface languages as a segmented control. Each language is named in
  // itself. The component never sets the language: Rust applies the choice and
  // every window hears `language_changed` on the UI channel.
  import { invoke } from '@tauri-apps/api/core';
  import { t } from './i18n';
  import { LANGS, lang, nativeName, type Lang } from './locale.svelte';

  /**
   * `compact`: for the first-run screen, without the title. The note about what
   * stays Russian is shown there only while the language is English: whoever was
   * given English by the language of Windows must learn it before the first card.
   */
  let { name, compact = false }: { name: string; compact?: boolean } = $props();

  let busy = $state(false);
  let error = $state('');

  async function choose(next: Lang) {
    if (busy || next === lang()) return;
    busy = true;
    error = '';
    try {
      await invoke('set_language', { value: next });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="lang" class:compact>
  {#if !compact}<div class="title">{t('lang.title')}</div>{/if}
  <div class="langs" role="radiogroup" aria-label={t('lang.title')}>
    {#each LANGS as l (l)}
      <button type="button" role="radio" aria-checked={lang() === l} class:on={lang() === l} disabled={busy} onclick={() => choose(l)}>
        {nativeName(l)}
      </button>
    {/each}
  </div>
  {#if !compact || lang() === 'en'}<p class="sub">{t('lang.sub', { name })}</p>{/if}
  {#if error}<p class="err">{error}</p>{/if}
</div>

<style>
  .lang {
    margin-bottom: 12px;
  }
  .lang.compact {
    margin: 18px 0 0;
  }
  .title {
    margin-bottom: 8px;
    font-size: 14px;
  }
  .langs {
    display: inline-flex;
    gap: 2px;
    padding: 3px;
    border-radius: 13px;
    border: 1px solid var(--line);
  }
  button {
    all: unset;
    box-sizing: border-box;
    padding: 6px 14px;
    border-radius: 10px;
    border: 1px solid transparent;
    font-size: 13px;
    font-weight: 600;
    color: var(--mute);
    cursor: pointer;
    white-space: nowrap;
    transition:
      color 0.2s,
      background 0.2s;
  }
  button:hover:not(:disabled):not(.on) {
    color: #fff;
  }
  button.on {
    color: #fff;
    border-color: var(--line2);
    background: color-mix(in srgb, var(--v) 34%, transparent);
  }
  button:disabled {
    cursor: default;
  }
  button:disabled:not(.on) {
    opacity: 0.5;
  }
  button:focus-visible {
    outline: 2px solid var(--v);
    outline-offset: 1px;
  }
  .sub {
    margin: 8px 0 0;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
  }
  .err {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
