<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import { conversationRows, type ConversationRow } from './chat';
  import { t } from './i18n';

  // The list of conversations (decision 0052): a panel that slides over the
  // chat. Rust keeps the list and does every action; this only draws it.
  let {
    onswitched,
    onclose,
  }: {
    /** Another conversation is open now: the chat reads its history again. */
    onswitched: () => void;
    onclose: () => void;
  } = $props();

  let rows = $state<ConversationRow[]>([]);
  let loaded = $state(false);
  let error = $state('');
  /** The row whose title is being edited, and the text in the field. */
  let editing = $state<string | null>(null);
  let draft = $state('');
  /** The row that asks whether to delete for good. */
  let sure = $state<string | null>(null);
  let busy = $state(false);
  let field: HTMLInputElement | undefined = $state();

  onMount(() => {
    run(() => invoke('list_conversations'));
  });

  const currentOf = (list: ConversationRow[]) => list.find((r) => r.current)?.id ?? '';

  /** Runs one command that answers with the list; tells the chat when the open conversation changed. */
  async function run(call: () => Promise<unknown>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = '';
    const before = currentOf(rows);
    try {
      rows = conversationRows(await call(), new Date());
      loaded = true;
      const now = currentOf(rows);
      if (before && now !== before) onswitched();
      return true;
    } catch (e) {
      error = String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function open(row: ConversationRow) {
    if (editing || row.current) {
      if (row.current) onclose();
      return;
    }
    if (await run(() => invoke('open_conversation', { id: row.id }))) onclose();
  }

  async function fresh() {
    const before = currentOf(rows);
    if (await run(() => invoke('new_conversation'))) {
      // «+» on an empty conversation keeps it: the chat is cleared all the same.
      if (currentOf(rows) === before) onswitched();
      onclose();
    }
  }

  async function edit(row: ConversationRow) {
    sure = null;
    editing = row.id;
    draft = row.title;
    await tick();
    field?.select();
  }

  async function save() {
    const id = editing;
    if (!id) return;
    if (await run(() => invoke('rename_conversation', { id, title: draft }))) editing = null;
  }

  function pin(row: ConversationRow) {
    run(() => invoke('pin_conversation', { id: row.id, pinned: !row.pinned }));
  }

  async function remove(row: ConversationRow) {
    if (sure !== row.id) {
      sure = row.id;
      editing = null;
      return;
    }
    sure = null;
    await run(() => invoke('delete_conversation', { id: row.id }));
  }

  function onkey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.stopPropagation();
    if (editing) editing = null;
    else if (sure) sure = null;
    else onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="veil" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && onclose()}>
  <section class="panel" aria-label={t('conv.title')}>
    <header>
      <b>{t('conv.title')}</b>
      <span class="grow"></span>
      <button class="bt pri new" title={t('conv.new_title')} onclick={fresh} disabled={busy}><Icon name="plus" size={14} />{t('conv.new')}</button>
      <button class="ib" title={t('conv.close')} aria-label={t('conv.close')} onclick={onclose}><Icon name="x" /></button>
    </header>
    {#if error}<p class="err">{error}</p>{/if}
    <ul>
      {#each rows as row (row.id)}
        <li class:current={row.current}>
          {#if editing === row.id}
            <form
              class="rename"
              onsubmit={(e) => {
                e.preventDefault();
                save();
              }}
            >
              <input bind:this={field} class="fld" bind:value={draft} maxlength="40" placeholder={t('conv.rename_placeholder')} aria-label={t('conv.rename')} />
              <button class="ib" type="submit" title={t('conv.save')} aria-label={t('conv.save')} disabled={busy}><Icon name="check" size={14} /></button>
              <button class="ib" type="button" title={t('general.cancel')} aria-label={t('general.cancel')} onclick={() => (editing = null)}><Icon name="x" size={14} /></button>
            </form>
          {:else}
            <button class="row" onclick={() => open(row)} aria-current={row.current ? 'true' : undefined}>
              <span class="name">
                {#if row.pinned}<span class="pinmark"><Icon name="pin" size={12} /></span>{/if}
                <span class="text" class:untitled={!row.title}>{row.title || t('conv.untitled')}</span>
              </span>
              <span class="meta">
                {#if row.current}<i class="now">{t('conv.open_now')}</i>{/if}
                <span>{row.when}</span>
              </span>
            </button>
            {#if sure === row.id}
              <div class="sure">
                <button class="bt no" onclick={() => remove(row)} disabled={busy}>{t('conv.delete_sure')}</button>
                <button class="bt" onclick={() => (sure = null)}>{t('general.cancel')}</button>
              </div>
            {:else}
              <div class="acts">
                <button class="ib" class:on={row.pinned} title={t(row.pinned ? 'conv.unpin' : 'conv.pin')} aria-label={t(row.pinned ? 'conv.unpin' : 'conv.pin')} aria-pressed={row.pinned} onclick={() => pin(row)}><Icon name="pin" size={14} /></button>
                <button class="ib" title={t('conv.rename')} aria-label={t('conv.rename')} onclick={() => edit(row)}><Icon name="pencil" size={14} /></button>
                <button class="ib" title={t('conv.delete')} aria-label={t('conv.delete')} onclick={() => remove(row)}><Icon name="trash" size={14} /></button>
              </div>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
    {#if loaded && rows.length === 0}<p class="none">{t('conv.empty')}</p>{/if}
  </section>
</div>

<style>
  .veil {
    position: absolute;
    inset: 0;
    z-index: 30;
    display: flex;
    justify-content: flex-end;
    background: rgba(0, 0, 0, 0.35);
    border-radius: inherit;
  }
  .panel {
    width: min(340px, 100%);
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border-left: 1px solid var(--line2);
    box-shadow: -14px 0 40px rgba(0, 0, 0, 0.45);
    animation: slide 0.18s ease-out;
  }
  @keyframes slide {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 10px 10px 16px;
    border-bottom: 1px solid var(--line);
  }
  header b {
    font-size: 14px;
  }
  .grow {
    flex: 1;
  }
  .new {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 8px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    position: relative;
    display: flex;
    align-items: center;
    gap: 4px;
    border-radius: 12px;
    padding-right: 4px;
  }
  li:hover,
  li:focus-within {
    background: rgba(255, 255, 255, 0.05);
  }
  li.current {
    background: color-mix(in srgb, var(--v) 14%, transparent);
  }
  .row {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 10px;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: 13.5px;
    font-weight: 600;
    color: var(--ink);
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .text.untitled {
    color: var(--mute);
    font-style: italic;
  }
  .pinmark {
    display: inline-flex;
    color: var(--v);
  }
  .meta {
    display: flex;
    gap: 8px;
    font-size: 11.5px;
    color: var(--dim);
  }
  .now {
    font-style: normal;
    font-weight: 700;
    color: var(--v);
  }
  .acts {
    display: flex;
    gap: 2px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  li:hover .acts,
  li:focus-within .acts,
  .acts:has(.on) {
    opacity: 1;
  }
  .acts .ib {
    width: 26px;
    height: 26px;
  }
  .acts .ib.on {
    color: var(--v);
  }
  .sure {
    display: flex;
    gap: 6px;
    padding: 4px 0;
  }
  .sure .bt {
    padding: 5px 10px;
    font-size: 12px;
  }
  .rename {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 0 4px 4px;
  }
  .rename .fld {
    flex: 1;
    min-width: 0;
    padding: 6px 10px;
    font-size: 13px;
  }
  .rename .ib {
    width: 26px;
    height: 26px;
  }
  .err,
  .none {
    margin: 10px 16px;
    font-size: 12.5px;
  }
  .err {
    color: var(--bad);
  }
  .none {
    color: var(--dim);
  }
</style>
