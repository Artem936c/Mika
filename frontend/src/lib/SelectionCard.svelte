<script lang="ts">
  import Icon from './Icon.svelte';
  import { t } from './i18n';
  import { PASTABLE, SELECTION_ACTIONS, type Selection } from './selection';

  let {
    selection,
    busy,
    onaction,
    onpaste,
    onclose,
  }: {
    selection: Selection;
    /** She is answering: the buttons wait. */
    busy: boolean;
    onaction: (action: string) => void;
    /** Resolves when the answer is in place; rejects with the reason. */
    onpaste: () => Promise<void>;
    onclose: () => void;
  } = $props();

  let chosen = $state('');
  let note = $state('');

  function pick(action: string) {
    chosen = action;
    note = '';
    onaction(action);
  }

  async function paste() {
    try {
      await onpaste();
      note = t('selection.pasted');
    } catch (e) {
      note = String(e);
    }
  }
</script>

<div class="sel">
  <div class="hd">
    <Icon name="term" size={14} />
    <span>{t('selection.title', { app: selection.app || '—' })}</span>
    <button class="x" title={t('selection.close')} aria-label={t('selection.close')} onclick={onclose}><Icon name="x" size={13} /></button>
  </div>
  <p class="txt">{selection.text}</p>
  <div class="acts">
    {#each SELECTION_ACTIONS as a (a)}
      <button class="bt" class:pri={chosen === a} disabled={busy} onclick={() => pick(a)}>{t(`selection.${a}`)}</button>
    {/each}
  </div>
  {#if PASTABLE.has(chosen) && !busy}
    <div class="acts">
      <button class="bt pri" onclick={paste}><Icon name="check" />{t('selection.paste')}</button>
      {#if note}<span class="note">{note}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .sel {
    flex: none;
    margin: 0 14px 10px;
    padding: 12px 14px;
    border-radius: 20px;
    background: var(--glass);
    border: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .hd {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--mute);
  }
  .hd span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    margin-left: auto;
    background: none;
    border: 0;
    color: var(--mute);
    cursor: pointer;
    padding: 2px;
    display: flex;
  }
  .txt {
    margin: 0;
    font-size: 13px;
    line-height: 1.4;
    max-height: 58px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .note {
    font-size: 12px;
    color: var(--mute);
  }
</style>
