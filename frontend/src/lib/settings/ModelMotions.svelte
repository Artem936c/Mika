<script lang="ts">
  // Animations the user brought in (decision 0040): add, rename, play, delete.
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import { t } from '../i18n';
  import { model } from '../model/state.svelte';

  interface Motion {
    id: string;
    name: string;
    ms: number;
  }

  let motions = $state<Motion[]>([]);
  let busy = $state(false);
  let error = $state('');
  /** The animation whose cross was pressed once: the second press deletes it. */
  let sure = $state('');
  let sureTimer: ReturnType<typeof setTimeout> | undefined;

  async function run<T>(command: string, args: Record<string, unknown> = {}): Promise<T | undefined> {
    try {
      const result = await invoke<T>(command, args);
      error = '';
      return result;
    } catch (e) {
      error = String(e);
      return undefined;
    }
  }

  async function reload() {
    motions = (await run<Motion[]>('avatar_motions')) ?? motions;
  }

  onMount(() => {
    reload();
    let stop: (() => void) | undefined;
    listen<{ action?: string }>('mika://ui', (e) => {
      if (e.payload?.action === 'avatar_motions_changed') reload();
    })
      .then((u) => (stop = u))
      .catch(() => {});
    return () => stop?.();
  });

  async function add() {
    if (busy) return;
    busy = true;
    try {
      await run('avatar_motion_add');
      await reload();
    } finally {
      busy = false;
    }
  }

  async function rename(m: Motion, name: string) {
    if (name.trim() === m.name) return;
    if ((await run('avatar_motion_rename', { id: m.id, name })) === undefined) await reload();
  }

  function remove(id: string) {
    clearTimeout(sureTimer);
    if (sure !== id) {
      sure = id;
      sureTimer = setTimeout(() => (sure = ''), 4000);
      return;
    }
    sure = '';
    run('avatar_motion_delete', { id });
  }
</script>

{#if model.id}
  <div class="lbl">{t('persona.motions')}</div>
  <p class="lead">{t('persona.motions_lead')}</p>
  <div class="list">
    {#each motions as m (m.id)}
      <div class="row">
        <button class="play" title={t('persona.motion_play')} aria-label={t('persona.motion_play')} onclick={() => run('avatar_motion_play', { id: m.id })}>▶</button>
        <input
          value={m.name}
          maxlength="60"
          spellcheck="false"
          aria-label={t('persona.motion_name')}
          onblur={(e) => rename(m, e.currentTarget.value)}
          onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
        />
        <span class="len">{t('persona.motion_seconds', { n: (m.ms / 1000).toFixed(1) })}</span>
        <button class="del" class:sure={sure === m.id} title={t('persona.motion_delete')} aria-label={t('persona.motion_delete')} onclick={() => remove(m.id)}>
          {sure === m.id ? t('persona.model_delete_confirm') : '×'}
        </button>
      </div>
    {/each}
    <button class="add" disabled={busy} onclick={add}>{busy ? t('persona.model_preparing') : t('persona.motion_add')}</button>
  </div>
  {#if error}<p class="err">{error}</p>{/if}
{/if}

<style>
  .lead {
    margin: -2px 0 8px;
    font-size: 12px;
    color: var(--mute);
  }
  .list {
    display: grid;
    gap: 6px;
    max-width: 420px;
  }
  .row {
    display: grid;
    grid-template-columns: 30px minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 8px;
  }
  input {
    box-sizing: border-box;
    min-width: 0;
    height: 30px;
    padding: 0 8px;
    border-radius: 9px;
    border: 1px solid var(--line2);
    background: #14131f;
    color: #fff;
    font: inherit;
    font-size: 12.5px;
  }
  input:focus {
    outline: none;
    border-color: rgba(143, 125, 255, 0.6);
  }
  button {
    all: unset;
    cursor: pointer;
    box-sizing: border-box;
    text-align: center;
    font-size: 12.5px;
    color: var(--mute);
  }
  button:hover {
    color: #fff;
  }
  .play {
    width: 30px;
    height: 30px;
    border-radius: 9px;
    border: 1px solid var(--line2);
  }
  .len {
    font-size: 12px;
    color: var(--mute);
  }
  .del {
    min-width: 22px;
    height: 22px;
    padding: 0 6px;
    border-radius: 11px;
    line-height: 22px;
  }
  .del.sure {
    font-size: 11px;
    font-weight: 700;
    color: #fff;
    background: var(--bad);
  }
  .add {
    height: 34px;
    border-radius: 10px;
    border: 1px dashed var(--line2);
    font-weight: 600;
  }
  .add:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .err {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
