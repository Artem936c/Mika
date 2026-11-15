<script lang="ts">
  // The avatar to show: the built-in one or a model the user brought in.
  import { invoke } from '@tauri-apps/api/core';
  import { t } from '../i18n';
  import { model, reloadModel, type ModelInfo } from '../model/state.svelte';

  let { name }: { name: string } = $props();

  let busy = $state(false);
  let error = $state('');
  /** The model whose cross was pressed once: the second press deletes it. */
  let sure = $state('');
  let sureTimer: ReturnType<typeof setTimeout> | undefined;

  const active = $derived(model.list.find((m) => m.id === model.id) ?? null);
  // A model chosen a moment ago has no pictures yet: the preview draws them.
  const preparing = $derived(model.id !== '' && model.failed === '' && !model.stillsComplete && !model.stillsFailed);

  async function run(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
    try {
      const result = await invoke(command, args);
      error = '';
      return result;
    } catch (e) {
      error = String(e);
      return undefined;
    }
  }

  async function add() {
    if (busy) return;
    busy = true;
    try {
      const added = (await run('avatar_model_add')) as ModelInfo | null | undefined;
      // Closed dialog or a refused file: nothing changes.
      if (added) await run('avatar_model_select', { id: added.id });
      await reloadModel().catch(() => {});
    } finally {
      busy = false;
    }
  }

  function select(id: string) {
    if (id !== model.id) run('avatar_model_select', { id });
  }

  function remove(id: string) {
    clearTimeout(sureTimer);
    if (sure !== id) {
      sure = id;
      sureTimer = setTimeout(() => (sure = ''), 4000);
      return;
    }
    sure = '';
    run('avatar_model_delete', { id });
  }

  const megabytes = (bytes: number) => t('persona.model_mb', { n: Math.max(1, Math.round(bytes / 1048576)) });
  const failure = $derived(
    model.failed === 'webgl' || model.failed === 'load' || model.failed === 'context_lost' ? t(`persona.model_fail_${model.failed}`) : '',
  );
</script>

<div class="mods">
  <button class="tile" class:on={model.id === ''} aria-pressed={model.id === ''} onclick={() => select('')}>
    <b>{name}</b><span>{t('persona.model_default')}</span>
  </button>
  {#each model.list as m (m.id)}
    <div class="tile" class:on={m.id === model.id}>
      <button class="pick" aria-pressed={m.id === model.id} onclick={() => select(m.id)}>
        <b>{m.title || t('persona.model_untitled')}</b>
        <span>{m.id === model.id && preparing ? t('persona.model_preparing') : megabytes(m.size_bytes)}</span>
      </button>
      <button class="del" class:sure={sure === m.id} title={t('persona.model_delete')} aria-label={t('persona.model_delete')} onclick={() => remove(m.id)}>
        {sure === m.id ? t('persona.model_delete_confirm') : '×'}
      </button>
    </div>
  {/each}
  <button class="tile add" disabled={busy} onclick={add}>{busy ? t('persona.model_preparing') : t('persona.model_add')}</button>
</div>
{#if active}
  <p class="about">
    {#if active.author}{t('persona.model_by', { author: active.author })}{/if}
    {#if active.license}<span>{t('persona.model_license', { license: active.license })}</span>{/if}
  </p>
{/if}
{#if model.failed}<p class="err">{t('persona.model_failed')} {failure}</p>{/if}
{#if model.stillsFailed}<p class="err">{t('persona.model_stills_failed')}</p>{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .mods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: 10px;
  }
  .tile {
    all: unset;
    box-sizing: border-box;
    position: relative;
    min-width: 0;
    height: 64px;
    border-radius: 14px;
    border: 1px solid var(--line2);
    display: grid;
    place-items: center;
    align-content: center;
    padding: 0 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--mute);
    text-align: center;
    line-height: 1.35;
    transition:
      border-color 0.2s,
      color 0.2s;
  }
  button.tile,
  .pick {
    cursor: pointer;
  }
  button.tile:hover,
  .tile:hover {
    color: #fff;
  }
  .tile.on {
    border-color: var(--v);
    box-shadow: 0 0 0 3px rgba(143, 125, 255, 0.2);
    color: #fff;
  }
  .tile b,
  .tile span {
    display: block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tile span {
    font-weight: 500;
    color: var(--mute);
  }
  .pick {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    width: 100%;
    height: 100%;
    display: grid;
    place-items: center;
    align-content: center;
    padding: 0 10px;
  }
  .del {
    all: unset;
    cursor: pointer;
    position: absolute;
    top: 3px;
    right: 3px;
    min-width: 18px;
    height: 18px;
    padding: 0 4px;
    box-sizing: border-box;
    border-radius: 9px;
    text-align: center;
    font-size: 13px;
    line-height: 18px;
    color: var(--mute);
    opacity: 0;
    transition: opacity 0.2s;
  }
  .tile:hover .del,
  .del:focus-visible,
  .del.sure {
    opacity: 1;
  }
  .del:hover {
    color: #fff;
  }
  .del.sure {
    font-size: 11px;
    font-weight: 700;
    color: #fff;
    background: var(--bad);
  }
  .add {
    border-style: dashed;
  }
  .add:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .about {
    margin: 8px 0 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--mute);
    overflow-wrap: anywhere;
  }
  .about span {
    display: block;
  }
  .err {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
