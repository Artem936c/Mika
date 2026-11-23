<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import Icon from './Icon.svelte';
  import { t } from './i18n';
  import { personaLabel, type PersonaView } from './persona';

  // The quick switch of her character in the chat header (decision 0051).
  // Rust keeps the list and the choice; this only draws them.
  let { onedit }: { /** Opens the character page of the settings. */ onedit: () => void } = $props();

  let open = $state(false);
  let view = $state<PersonaView | null>(null);
  let error = $state('');
  let box: HTMLDivElement | undefined = $state();

  async function toggle() {
    open = !open;
    if (!open) return;
    error = '';
    try {
      view = await invoke<PersonaView>('get_personas');
    } catch (e) {
      error = String(e);
    }
  }

  async function choose(choice: string) {
    if (view?.choice !== choice) {
      try {
        view = await invoke<PersonaView>('set_persona', { choice });
      } catch (e) {
        error = String(e);
        return;
      }
    }
    open = false;
  }

  function edit() {
    open = false;
    onedit();
  }

  function outside(e: PointerEvent) {
    if (open && box && !box.contains(e.target as Node)) open = false;
  }
</script>

<svelte:window onpointerdown={outside} onkeydown={(e) => open && e.key === 'Escape' && (e.stopPropagation(), (open = false))} />

<div class="pm" bind:this={box}>
  <button class="ib" class:on={open} title={t('chat.persona_title')} aria-label={t('chat.persona')} aria-haspopup="menu" aria-expanded={open} onclick={toggle}>
    <Icon name="smile" />
  </button>
  {#if open}
    <div class="menu" role="menu" aria-label={t('chat.persona')}>
      {#if view}
        {@const v = view}
        {#each v.ready as p (p)}
          <button role="menuitemradio" aria-checked={v.choice === p} class:on={v.choice === p} onclick={() => choose(p)}>
            <span>{personaLabel(p, v.ready)}</span>
            {#if v.choice === p}<Icon name="check" size={13} />{/if}
          </button>
        {/each}
        {#if v.custom.length}
          <div class="sep">{t('chat.persona_own')}</div>
          {#each v.custom as c (c.name)}
            <button role="menuitemradio" aria-checked={v.choice === c.name} class:on={v.choice === c.name} onclick={() => choose(c.name)}>
              <span>{c.name}</span>
              {#if v.choice === c.name}<Icon name="check" size={13} />{/if}
            </button>
          {/each}
        {/if}
      {/if}
      {#if error}<p class="err">{error}</p>{/if}
      <button class="edit" role="menuitem" onclick={edit}>{t('chat.persona_edit')}</button>
    </div>
  {/if}
</div>

<style>
  .pm {
    position: relative;
    flex: none;
  }
  .menu {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 20;
    min-width: 190px;
    max-height: min(420px, calc(100vh - 80px));
    overflow-y: auto;
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    border-radius: 16px;
    background: var(--panel);
    border: 1px solid var(--line2);
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.5);
  }
  .menu button {
    all: unset;
    box-sizing: border-box;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 7px 10px;
    border-radius: 10px;
    font-size: 13px;
    font-weight: 600;
    color: var(--mute);
  }
  .menu button span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .menu button:hover,
  .menu button:focus-visible {
    color: var(--ink);
    background: rgba(255, 255, 255, 0.06);
  }
  .menu button.on {
    color: var(--v);
  }
  .sep {
    margin: 6px 10px 2px;
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .menu .edit {
    margin-top: 4px;
    border-top: 1px solid var(--line);
    border-radius: 0 0 10px 10px;
    font-weight: 700;
  }
  .err {
    margin: 4px 10px;
    font-size: 12px;
    color: var(--bad);
  }
</style>
