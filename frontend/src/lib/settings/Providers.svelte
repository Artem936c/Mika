<script lang="ts">
  import ModelPicker from './ModelPicker.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import MediaProviders from './MediaProviders.svelte';
  import { formatSeconds } from '../chat';
  import { t } from '../i18n';

  type Kind = 'openai' | 'anthropic' | 'ollama';
  interface LlmSettings {
    kind: Kind;
    base_url: string;
    default_base_url: string;
    model: string;
    has_key: boolean;
  }

  const KINDS: { id: Kind; url: string }[] = [
    { id: 'openai', url: 'https://api.openai.com/v1' },
    { id: 'anthropic', url: 'https://api.anthropic.com' },
    { id: 'ollama', url: 'http://localhost:11434/v1' },
  ];


  let kind = $state<Kind>('openai');
  let baseUrl = $state('');
  let model = $state('');
  let key = $state('');
  let hasKey = $state(false);
  let status = $state('');
  let statusKind = $state<'ok' | 'bad' | ''>('');
  let saving = $state(false);
  /** Milliseconds to the first chunk from the last check. */
  let latency = $state<number | null>(null);

  const placeholderUrl = $derived(KINDS.find((k) => k.id === kind)?.url ?? '');
  const needKey = $derived(!hasKey && kind !== 'ollama');

  onMount(async () => {
    const s = await invoke<LlmSettings>('get_llm_settings');
    kind = s.kind;
    baseUrl = s.base_url;
    model = s.model;
    hasKey = s.has_key;
  });

  function fail(e: unknown) {
    status = String(e);
    statusKind = 'bad';
  }

  async function save(): Promise<boolean> {
    saving = true;
    latency = null;
    try {
      await invoke('save_llm_settings', { kind, baseUrl, model, apiKey: key ? key : null });
      if (key) hasKey = true;
      key = '';
      return true;
    } catch (e) {
      fail(e);
      return false;
    } finally {
      saving = false;
    }
  }

  async function onsave() {
    if (await save()) {
      status = t('providers.saved');
      statusKind = 'ok';
    }
  }

  async function forgetKey() {
    try {
      await invoke('save_llm_settings', { kind, baseUrl, model, apiKey: '' });
      hasKey = false;
      latency = null;
      status = t('providers.forgot');
      statusKind = 'ok';
    } catch (e) {
      fail(e);
    }
  }

  async function check() {
    if (!(await save())) return;
    status = t('providers.checking');
    statusKind = '';
    try {
      latency = await invoke<number>('test_provider');
      status = '';
    } catch (e) {
      fail(e);
    }
  }
</script>

<div class="prov">
  <div class="ico"><Icon name="star" size={19} /></div>
  <div class="what">
    <b>{t('providers.llm')}</b>
    <small>{t(`providers.kind.${kind}`)}{model ? ` · ${model}` : ''}</small>
  </div>
  {#if latency !== null}
    <span class="lat ok">{t('providers.latency', { value: formatSeconds(latency) })}</span>
  {:else if needKey}
    <span class="lat warn">{t('providers.need_key')}</span>
  {:else}
    <span class="lat">{t('providers.unchecked')}</span>
  {/if}
</div>

<div class="form">
  <div class="seg" role="radiogroup" aria-label={t('providers.kind_label')}>
    {#each KINDS as k (k.id)}
      <button role="radio" aria-checked={kind === k.id} class:on={kind === k.id} onclick={() => (kind = k.id)}>
        {t(`providers.kind.${k.id}`)}
      </button>
    {/each}
  </div>
  <label>
    <span class="lbl">{t('providers.url')}</span>
    <input class="fld" bind:value={baseUrl} placeholder={placeholderUrl} spellcheck="false" />
  </label>
  <div class="lab">
    <span class="lbl">{t('providers.model')}</span>
    <ModelPicker bind:value={model} purpose="chat" {baseUrl} {kind} placeholder="gpt-4o-mini" />
  </div>
  <label>
    <span class="lbl">{t('providers.key')}</span>
    <input
      class="fld"
      type="password"
      bind:value={key}
      placeholder={hasKey ? t('providers.key_saved') : kind === 'ollama' ? t('providers.key_none') : 'sk-…'}
      autocomplete="off"
    />
  </label>
  <p class="hint">{t('providers.key_hint')}</p>
  <div class="row">
    <button class="bt pri" onclick={onsave} disabled={saving}>{t('providers.save')}</button>
    <button class="bt" onclick={check} disabled={saving}>{t('providers.check')}</button>
    {#if hasKey}<button class="bt no" onclick={forgetKey}>{t('providers.forget')}</button>{/if}
    {#if status}<span class="status {statusKind}">{status}</span>{/if}
  </div>
</div>

<MediaProviders />

<p class="hint">{t('providers.speech_hint')}</p>

<style>
  .prov {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px 16px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    margin-bottom: 10px;
  }
  .ico {
    flex: none;
    width: 40px;
    height: 40px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: rgba(var(--v-rgb), 0.16);
    color: var(--v);
  }
  .what {
    min-width: 0;
  }
  .what b {
    display: block;
    font-size: 14px;
  }
  .what small {
    display: block;
    color: var(--mute);
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lat {
    margin-left: auto;
    flex: none;
    font-size: 12px;
    font-weight: 700;
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
    padding: 5px 10px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .lat.ok {
    color: var(--ok);
    background: rgba(95, 227, 161, 0.1);
  }
  .lat.warn {
    color: var(--warn);
    background: rgba(255, 198, 92, 0.1);
  }
  .form {
    margin: 0 0 14px;
    padding: 16px;
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
    margin: 14px 0 6px;
  }
  .hint {
    margin: 8px 0 14px;
    font-size: 12px;
    color: var(--dim);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .status {
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
