<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Permissions from './Permissions.svelte';
  import General from './General.svelte';

  let { onback }: { onback: () => void } = $props();

  type Kind = 'openai' | 'anthropic' | 'ollama';
  interface LlmSettings {
    kind: Kind;
    base_url: string;
    default_base_url: string;
    model: string;
    has_key: boolean;
  }

  const KINDS: { id: Kind; label: string; url: string }[] = [
    { id: 'openai', label: 'OpenAI-совместимый', url: 'https://api.openai.com/v1' },
    { id: 'anthropic', label: 'Anthropic', url: 'https://api.anthropic.com' },
    { id: 'ollama', label: 'Ollama', url: 'http://localhost:11434/v1' },
  ];

  let kind = $state<Kind>('openai');
  let baseUrl = $state('');
  let model = $state('');
  let key = $state('');
  let hasKey = $state(false);
  let status = $state('');
  let statusKind = $state<'ok' | 'bad' | ''>('');
  let saving = $state(false);
  let tab = $state<'provider' | 'permissions' | 'tools' | 'general'>('provider');
  let webEnabled = $state(false);

  async function toggleWeb(v: boolean) {
    try {
      await invoke('set_web_enabled', { value: v });
      webEnabled = v;
    } catch (e) {
      status = String(e);
      statusKind = 'bad';
    }
  }
  const placeholderUrl = $derived(KINDS.find((k) => k.id === kind)?.url ?? '');

  onMount(async () => {
    const s = await invoke<LlmSettings>('get_llm_settings');
    kind = s.kind;
    baseUrl = s.base_url;
    model = s.model;
    hasKey = s.has_key;
    const ui = await invoke<{ web_enabled: boolean }>('get_ui_settings');
    webEnabled = ui.web_enabled;
  });

  async function save(): Promise<boolean> {
    saving = true;
    try {
      await invoke('save_llm_settings', {
        kind,
        baseUrl,
        model,
        apiKey: key ? key : null,
      });
      if (key) hasKey = true;
      key = '';
      return true;
    } catch (e) {
      status = String(e);
      statusKind = 'bad';
      return false;
    } finally {
      saving = false;
    }
  }

  async function onsave() {
    if (await save()) {
      status = 'Сохранено';
      statusKind = 'ok';
    }
  }

  async function forgetKey() {
    await invoke('save_llm_settings', { kind, baseUrl, model, apiKey: '' });
    hasKey = false;
    status = 'Ключ удалён';
    statusKind = 'ok';
  }

  async function check() {
    if (!(await save())) return;
    status = 'Проверяю…';
    statusKind = '';
    try {
      const ms = await invoke<number>('test_provider');
      status = `Работает, первый ответ через ${ms} мс`;
      statusKind = 'ok';
    } catch (e) {
      status = String(e);
      statusKind = 'bad';
    }
  }
</script>

<div class="settings">
  <header>
    <button class="ib" onclick={onback} aria-label="Назад">←</button>
    <b>Настройки</b>
    <nav class="tabs">
      <button class:on={tab === 'provider'} onclick={() => (tab = 'provider')}>Провайдер</button>
      <button class:on={tab === 'permissions'} onclick={() => (tab = 'permissions')}>Разрешения</button>
      <button class:on={tab === 'tools'} onclick={() => (tab = 'tools')}>Инструменты</button>
      <button class:on={tab === 'general'} onclick={() => (tab = 'general')}>Общие</button>
    </nav>
  </header>
  <div class="body">
    {#if tab === 'permissions'}
      <Permissions />
    {:else if tab === 'general'}
      <General />
    {:else if tab === 'tools'}
      <label class="switch">
        <input type="checkbox" checked={webEnabled} onchange={(e) => toggleWeb(e.currentTarget.checked)} />
        <span>
          <b>Интернет-инструменты</b>
          Поиск, чтение страниц и погода. Запросы уходят в DuckDuckGo и Open-Meteo. Открыть ссылку в браузере можно и без этого.
        </span>
      </label>
      <p class="hint">Микрофон и просмотр экрана появятся в следующих эпиках и тоже будут выключены, пока ты их не включишь.</p>
      {#if status}<p class="status {statusKind}">{status}</p>{/if}
    {:else}
    <div class="seg" role="radiogroup" aria-label="Тип провайдера">
      {#each KINDS as k}
        <button role="radio" aria-checked={kind === k.id} class:on={kind === k.id} onclick={() => (kind = k.id)}>{k.label}</button>
      {/each}
    </div>

    <label>
      <span>Адрес</span>
      <input bind:value={baseUrl} placeholder={placeholderUrl} spellcheck="false" />
    </label>
    <label>
      <span>Модель</span>
      <input bind:value={model} placeholder="gpt-4o-mini" spellcheck="false" />
    </label>
    <label>
      <span>Ключ</span>
      <input
        type="password"
        bind:value={key}
        placeholder={hasKey ? 'сохранён в диспетчере учётных данных' : kind === 'ollama' ? 'не нужен' : 'sk-…'}
        autocomplete="off"
      />
    </label>
    <p class="hint">Ключ хранится в диспетчере учётных данных Windows и не попадает в файлы настроек.</p>

    <div class="row">
      <button class="bt pri" onclick={onsave} disabled={saving}>Сохранить</button>
      <button class="bt" onclick={check} disabled={saving}>Проверить</button>
      {#if hasKey}<button class="bt no" onclick={forgetKey}>Удалить ключ</button>{/if}
    </div>
    {#if status}<p class="status {statusKind}">{status}</p>{/if}
    {/if}
  </div>
</div>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    height: 100%;
    animation: fade 0.3s var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  header {
    height: 54px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 14px;
    border-bottom: 1px solid var(--line);
    font-size: 14px;
  }
  .tabs {
    margin-left: auto;
    display: flex;
    gap: 4px;
  }
  .tabs button {
    all: unset;
    cursor: pointer;
    padding: 6px 11px;
    border-radius: 9px;
    font-size: 12.5px;
    font-weight: 700;
    color: var(--mute);
  }
  .tabs button.on {
    color: var(--ink);
    background: rgba(143, 125, 255, 0.22);
  }
  .ib {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid var(--line);
    color: var(--mute);
    cursor: pointer;
  }
  .body {
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .seg {
    display: inline-flex;
    align-self: flex-start;
    padding: 4px;
    border-radius: 14px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--line);
  }
  .seg button {
    all: unset;
    cursor: pointer;
    padding: 7px 12px;
    border-radius: 10px;
    font-size: 12.5px;
    font-weight: 700;
    color: var(--mute);
  }
  .seg button.on {
    color: var(--ink);
    background: rgba(143, 125, 255, 0.22);
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 12px;
    color: var(--mute);
  }
  input {
    height: 36px;
    padding: 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.04);
    outline: none;
    font-size: 13px;
  }
  input:focus {
    border-color: rgba(143, 125, 255, 0.6);
  }
  .switch {
    flex-direction: row;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    font-size: 12.5px;
    line-height: 1.5;
  }
  .switch input {
    width: 18px;
    height: 18px;
    margin-top: 2px;
    accent-color: var(--v);
  }
  .switch b {
    display: block;
    color: var(--ink);
    font-size: 13.5px;
  }
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--dim);
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .bt {
    height: 34px;
    padding: 0 14px;
    border-radius: 11px;
    font-size: 13px;
    font-weight: 700;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
    cursor: pointer;
  }
  .bt.pri {
    border: 0;
    background: linear-gradient(135deg, var(--v), #6b58e6);
  }
  .bt.no {
    color: var(--bad);
  }
  .bt:disabled {
    opacity: 0.4;
  }
  .status {
    margin: 0;
    font-size: 12.5px;
    color: var(--mute);
    word-break: break-word;
  }
  .status.ok {
    color: var(--ok);
  }
  .status.bad {
    color: var(--bad);
  }
</style>
