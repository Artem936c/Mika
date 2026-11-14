<script lang="ts">
  // The deep search block of the "Tools" page: the model, the report folder
  // and the limits of one search. The core checks and clamps what is sent.
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import ModelPicker from './ModelPicker.svelte';
  import { t } from '../i18n';
  import { MINUTES, PAGES, SEARCHES, TOKENS_K, researchArgs, type ResearchView } from '../research';

  let s = $state<ResearchView | null>(null);
  let folder = $state('');
  let model = $state('');
  let searches = $state(5);
  let pages = $state(8);
  let minutes = $state(5);
  let tokensK = $state(50);
  let note = $state('');
  let error = $state('');

  onMount(load);

  /** The fields take what the core keeps. */
  function show(view: ResearchView) {
    s = view;
    folder = view.folder;
    model = view.model;
    searches = view.max_searches;
    pages = view.max_pages;
    minutes = view.max_minutes;
    tokensK = Math.round(view.max_tokens / 1000);
  }

  async function load() {
    try {
      show(await invoke<ResearchView>('get_research_settings'));
    } catch (e) {
      error = String(e);
    }
  }

  async function save() {
    if (!s) return;
    try {
      show(await invoke<ResearchView>('set_research_settings', researchArgs(s, { folder, model, searches, pages, minutes, tokensK })));
      error = '';
      note = t('island.saved');
    } catch (e) {
      error = String(e);
      note = '';
    }
  }
</script>

{#if s}
  <div class="lbl">{t('research.title')}</div>
  <p class="hint">{t('research.sub')}</p>

  <div class="sub">{t('research.model')}</div>
  <ModelPicker bind:value={model} purpose="chat" placeholder={t('research.model_hint')} />
  <p class="hint under">{t('research.model_sub')}</p>

  <div class="sub">{t('research.folder')}</div>
  <div class="line">
    <input
      class="fld wide"
      bind:value={folder}
      spellcheck="false"
      placeholder={t('research.folder_hint', { path: s.folder_default })}
      oninput={() => (note = '')}
      onkeydown={(e) => e.key === 'Enter' && save()}
    />
  </div>
  {#if s.folder}<p class="hint under path">{s.folder_shown}</p>{/if}

  <div class="limits">
    <label class="line time">
      <span>{t('research.searches')}</span>
      <input class="fld" type="number" min={SEARCHES[0]} max={SEARCHES[1]} bind:value={searches} oninput={() => (note = '')} />
    </label>
    <label class="line time">
      <span>{t('research.pages')}</span>
      <input class="fld" type="number" min={PAGES[0]} max={PAGES[1]} bind:value={pages} oninput={() => (note = '')} />
    </label>
    <label class="line time">
      <span>{t('research.minutes')}</span>
      <input class="fld" type="number" min={MINUTES[0]} max={MINUTES[1]} bind:value={minutes} oninput={() => (note = '')} />
    </label>
    <label class="line time">
      <span>{t('research.tokens')}</span>
      <input class="fld" type="number" min={TOKENS_K[0]} max={TOKENS_K[1]} step="10" bind:value={tokensK} oninput={() => (note = '')} />
    </label>
  </div>
  <div class="line acts">
    <button class="bt" onclick={save}>{t('tools.save')}</button>
    {#if note}<span class="ok">{note}</span>{/if}
  </div>

  <p class="hint">{t('research.calls', { n: s.llm_calls, model: s.model_shown })} {t('research.estimate')}</p>
  {#if s.high_tokens}<p class="warn">{t('research.high')}</p>{/if}
  {#if s.high_folder}<p class="warn">{t('research.folder_high')}</p>{/if}
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .wide {
    flex: 1;
    min-width: 0;
  }
  .sub {
    margin: 12px 2px 6px;
    font-size: 12.5px;
    color: var(--mute);
  }
  .limits {
    display: grid;
    gap: 2px;
    margin-top: 10px;
  }
  .time {
    justify-content: space-between;
    margin: 4px 2px;
    font-size: 13px;
  }
  .time .fld {
    width: 90px;
  }
  .acts {
    margin: 10px 0;
  }
  .hint {
    margin: 4px 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .under {
    margin-top: 6px;
  }
  .path {
    word-break: break-all;
    user-select: text;
  }
  .ok {
    font-size: 12.5px;
    color: var(--ok);
  }
  .warn {
    margin: 4px 2px 10px;
    font-size: 12.5px;
    color: var(--warn);
  }
  .err {
    margin: 6px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
