<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import Avatar from './Avatar.svelte';
  import { dragRegion } from './drag';
  import General from './General.svelte';
  import Icon from './Icon.svelte';
  import Permissions from './Permissions.svelte';
  import type { Emotion } from './avatar';
  import { t, type MessageKey } from './i18n';
  import type { IconName } from './icons';
  import Costs from './settings/Costs.svelte';
  import Extensions from './settings/Extensions.svelte';
  import Hotkeys from './settings/Hotkeys.svelte';
  import IslandPage from './settings/IslandPage.svelte';
  import Journal from './settings/Journal.svelte';
  import Memory from './settings/Memory.svelte';
  import Persona from './settings/Persona.svelte';
  import Providers from './settings/Providers.svelte';
  import Tools from './settings/Tools.svelte';
  import Voice from './settings/Voice.svelte';
  import { changedByTalk } from './settings_talk';

  type Page = 'persona' | 'providers' | 'permissions' | 'tools' | 'memory' | 'extensions' | 'voice' | 'island' | 'hotkeys' | 'costs' | 'journal' | 'general';

  let {
    name,
    emotion,
    start = 'persona',
    onback,
  }: { name: string; emotion: Emotion; start?: Page; onback: () => void } = $props();

  const PAGES: { id: Page; icon: IconName; lead: MessageKey }[] = [
    { id: 'persona', icon: 'star', lead: 'persona.lead' },
    { id: 'providers', icon: 'plug', lead: 'providers.lead' },
    { id: 'permissions', icon: 'shield', lead: 'permissions.lead' },
    { id: 'tools', icon: 'wrench', lead: 'tools.lead' },
    { id: 'memory', icon: 'brain', lead: 'memory.lead' },
    { id: 'extensions', icon: 'puzzle', lead: 'ext.lead' },
    { id: 'voice', icon: 'vol', lead: 'voice.lead' },
    { id: 'island', icon: 'mon', lead: 'island.lead' },
    { id: 'hotkeys', icon: 'kbd', lead: 'hotkeys.lead' },
    { id: 'costs', icon: 'coins', lead: 'costs.lead' },
    { id: 'journal', icon: 'log', lead: 'journal.lead' },
    { id: 'general', icon: 'set', lead: 'general.lead' },
  ];

  // svelte-ignore state_referenced_locally
  let page = $state<Page>(start);
  const current = $derived(PAGES.find((p) => p.id === page) ?? PAGES[0]);
  // A setting changed by conversation: the open page is drawn anew with it (decision 0046).
  let changes = $state(0);
  onMount(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    listen('mika://ui', (e) => {
      if (changedByTalk(e.payload)) changes += 1;
    }).then((u) => (gone ? u() : (stop = u)));
    return () => {
      gone = true;
      stop?.();
    };
  });
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onback()} />

<div class="app">
  <aside class="side">
    <div class="me" use:dragRegion title={t('chat.drag_title')}>
      <button class="ib" onclick={onback} title={t('settings.back')} aria-label={t('settings.back')}><Icon name="back" /></button>
      <Avatar view="head" {emotion} size={32} />
      <b>{name}</b>
    </div>
    <nav>
      {#each PAGES as p (p.id)}
        <button class:on={page === p.id} aria-current={page === p.id ? 'page' : undefined} onclick={() => (page = p.id)}>
          <Icon name={p.icon} />{t(`settings.page.${p.id}`)}
        </button>
      {/each}
    </nav>
  </aside>

  <section class="page">
    <h3 use:dragRegion title={t('chat.drag_title')}>{t(`settings.page.${page}`)}</h3>
    <p class="lead">{t(current.lead)}</p>
    {#key `${page}:${changes}`}
      <div class="body">
        {#if page === 'persona'}
          <Persona />
        {:else if page === 'providers'}
          <Providers />
        {:else if page === 'permissions'}
          <Permissions {name} />
        {:else if page === 'tools'}
          <Tools />
        {:else if page === 'memory'}
          <Memory />
        {:else if page === 'extensions'}
          <Extensions />
        {:else if page === 'voice'}
          <Voice {name} />
        {:else if page === 'island'}
          <IslandPage />
        {:else if page === 'hotkeys'}
          <Hotkeys />
        {:else if page === 'costs'}
          <Costs />
        {:else if page === 'journal'}
          <Journal />
        {:else}
          <General {name} />
        {/if}
      </div>
    {/key}
  </section>
</div>

<style>
  .app {
    display: grid;
    grid-template-columns: 200px 1fr;
    width: 100%;
    height: 100%;
    text-align: left;
    background: var(--panel);
  }
  .side {
    background: rgba(255, 255, 255, 0.025);
    border-right: 1px solid var(--line);
    padding: 14px 12px;
    min-height: 0;
    overflow: clip;
    display: flex;
    flex-direction: column;
  }
  /* The list of pages scrolls when the window is too low for all of them. */
  nav {
    min-height: 0;
    overflow-y: auto;
    scrollbar-width: thin;
  }
  .me {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 2px 14px;
    font-size: 14px;
  }
  .me b {
    font-weight: 800;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  nav button {
    all: unset;
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    box-sizing: border-box;
    padding: 6px 12px;
    border-radius: 12px;
    font-size: 13.5px;
    font-weight: 600;
    color: var(--mute);
    margin-bottom: 1px;
    transition: color 0.2s;
  }
  nav button:hover:not(:disabled) {
    color: #fff;
  }
  nav button.on {
    color: #fff;
    background: rgba(var(--v-rgb), 0.18);
  }
  /* A size container: a page can ask how tall the room is (the journal sizes its list by it). */
  .page {
    container-type: size;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    padding: 22px 26px 24px;
  }
  h3 {
    margin: 0 0 4px;
    font-size: 19px;
    font-weight: 800;
  }
  .lead {
    margin: 0 0 18px;
    color: var(--mute);
    font-size: 13.5px;
  }
  .body {
    animation: in 0.3s var(--ease);
  }
  @keyframes in {
    from {
      opacity: 0;
      translate: 0 6px;
    }
  }
</style>
