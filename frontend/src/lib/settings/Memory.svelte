<script lang="ts">
  import ModelPicker from './ModelPicker.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { t } from '../i18n';

  interface Fact {
    id: number;
    created_at: string;
    text: string;
  }
  interface MemoryView {
    facts: Fact[];
    /** Conversations that are neither pinned nor open (decision 0052). */
    other_conversations: number;
    /** Conversations older than this many days are deleted; 0 is never. */
    keep_days: number;
    embedding_base_url: string;
    embedding_model: string;
    has_embedding_key: boolean;
    llm_base_url: string;
  }

  let m = $state<MemoryView | null>(null);
  let draft = $state('');
  let embedUrl = $state('');
  let embedModel = $state('');
  let embedKey = $state('');
  let status = $state('');
  let statusKind = $state<'ok' | 'bad' | ''>('');
  let sureFacts = $state(false);
  let sureOthers = $state(false);
  /** The choices of the cleanup; Rust refuses any other. */
  const KEEP = [7, 30, 90, 365, 0] as const;
  let busy = $state(false);

  onMount(load);

  async function load() {
    try {
      m = await invoke<MemoryView>('get_memory');
      embedUrl = m.embedding_base_url;
      embedModel = m.embedding_model;
    } catch (e) {
      fail(e);
    }
  }

  function fail(e: unknown) {
    status = String(e);
    statusKind = 'bad';
  }

  async function add() {
    if (!draft.trim()) return;
    try {
      await invoke('add_fact', { text: draft });
      draft = '';
      status = '';
      await load();
    } catch (e) {
      fail(e);
    }
  }

  async function remove(id: number) {
    await invoke('remove_fact', { id }).catch(fail);
    await load();
  }

  async function clearFacts() {
    if (!sureFacts) {
      sureFacts = true;
      return;
    }
    sureFacts = false;
    await invoke('clear_facts').catch(fail);
    await load();
  }

  async function setKeep(days: number) {
    try {
      await invoke('set_history_keep', { days });
      status = t('island.saved');
      statusKind = 'ok';
      await load();
    } catch (e) {
      fail(e);
    }
  }

  async function deleteOthers() {
    if (!sureOthers) {
      sureOthers = true;
      return;
    }
    sureOthers = false;
    try {
      const count = await invoke<number>('delete_unpinned_conversations');
      status = t('memory.deleted', { count });
      statusKind = 'ok';
    } catch (e) {
      fail(e);
    }
    await load();
  }

  async function saveEmbedding(): Promise<boolean> {
    busy = true;
    try {
      await invoke('save_embedding_settings', { baseUrl: embedUrl, model: embedModel, apiKey: embedKey ? embedKey : null });
      embedKey = '';
      await load();
      status = t('island.saved');
      statusKind = 'ok';
      return true;
    } catch (e) {
      fail(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function checkEmbedding() {
    if (!(await saveEmbedding())) return;
    status = t('providers.checking');
    statusKind = '';
    try {
      const size = await invoke<number>('test_embedding');
      status = t('memory.embed_ok', { size });
      statusKind = 'ok';
    } catch (e) {
      fail(e);
    }
  }
</script>

{#if m}
  <div class="lbl">{t('memory.facts')}</div>
  {#if m.facts.length === 0}
    <p class="none">{t('memory.none')}</p>
  {:else}
    <ul class="facts">
      {#each m.facts as f (f.id)}
        <li>
          <span>{f.text}</span>
          <button class="ib" title={t('memory.forget')} aria-label={t('memory.forget')} onclick={() => remove(f.id)}><Icon name="x" size={14} /></button>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="line">
    <input class="fld" bind:value={draft} maxlength="500" placeholder={t('memory.add_placeholder')} onkeydown={(e) => e.key === 'Enter' && add()} />
    <button class="bt" onclick={add} disabled={!draft.trim()}>{t('memory.add')}</button>
  </div>
  {#if m.facts.length > 0}
    <div class="acts">
      <button class="bt no" onclick={clearFacts}>{t(sureFacts ? 'memory.clear_sure' : 'memory.clear')}</button>
      {#if sureFacts}<button class="bt" onclick={() => (sureFacts = false)}>{t('general.cancel')}</button>{/if}
    </div>
  {/if}

  <div class="lbl">{t('memory.history')}</div>
  <p class="hint">{t('memory.history_sub')}</p>
  <label class="keep">
    <span>{t('memory.keep')}</span>
    <select class="fld" value={m.keep_days} onchange={(e) => setKeep(Number(e.currentTarget.value))}>
      {#each KEEP as d (d)}
        <option value={d}>{t(`memory.keep_${d}`)}</option>
      {/each}
    </select>
  </label>
  <p class="hint">{t('memory.keep_hint')}</p>
  {#if m.other_conversations > 0}
    <div class="acts">
      <button class="bt no" onclick={deleteOthers}>{t(sureOthers ? 'memory.clear_sure' : 'memory.delete_others', { count: m.other_conversations })}</button>
      {#if sureOthers}<button class="bt" onclick={() => (sureOthers = false)}>{t('general.cancel')}</button>{/if}
    </div>
  {/if}

  <div class="lbl">
    {t('memory.embed')}
    <span class="badge" class:on={!!m.embedding_model}>{t(m.embedding_model ? 'memory.embed_on' : 'memory.embed_off')}</span>
  </div>
  <p class="hint">{t('memory.embed_sub')}</p>
  <div class="form">
    <div class="lab">
      <span class="lbl">{t('memory.embed_model')}</span>
      <ModelPicker bind:value={embedModel} purpose="embedding" baseUrl={embedUrl} placeholder="text-embedding-3-small" />
    </div>
    <label>
      <span class="lbl">{t('memory.embed_url')}</span>
      <input class="fld" bind:value={embedUrl} placeholder={m.llm_base_url} spellcheck="false" />
    </label>
    <label>
      <span class="lbl">{t('memory.embed_key')}</span>
      <input class="fld" type="password" bind:value={embedKey} placeholder={m.has_embedding_key ? t('providers.key_saved') : ''} autocomplete="off" />
    </label>
    <div class="acts top">
      <button class="bt pri" onclick={saveEmbedding} disabled={busy}>{t('providers.save')}</button>
      <button class="bt" onclick={checkEmbedding} disabled={busy || !embedModel.trim()}>{t('providers.check')}</button>
    </div>
  </div>
{/if}
{#if status}<p class="status {statusKind}">{status}</p>{/if}

<style>
  .none,
  .hint {
    margin: 0 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .facts {
    list-style: none;
    margin: 0 0 10px;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  .facts li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 8px 8px 14px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    font-size: 13.5px;
  }
  .facts span {
    flex: 1;
    min-width: 0;
    user-select: text;
    word-break: break-word;
  }
  .facts .ib {
    width: 26px;
    height: 26px;
  }
  .line {
    display: flex;
    gap: 8px;
  }
  .keep {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 2px 6px;
    font-size: 13px;
  }
  .keep .fld {
    width: auto;
  }
  .acts {
    display: flex;
    gap: 8px;
    margin: 10px 0 0;
  }
  .acts.top {
    margin-top: 14px;
  }
  .lbl {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .badge {
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.06em;
    padding: 2px 8px;
    border-radius: 999px;
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
  }
  .badge.on {
    color: var(--ok);
    background: rgba(95, 227, 161, 0.1);
  }
  .form {
    padding: 4px 16px 16px;
    border-radius: 16px;
    border: 1px solid var(--line);
  }
  label,
  .lab {
    display: block;
  }
  label .lbl,
  .lab .lbl {
    display: block;
    margin: 12px 0 6px;
  }
  .status {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--mute);
    user-select: text;
  }
  .status.ok {
    color: var(--ok);
  }
  .status.bad {
    color: var(--bad);
  }
</style>
