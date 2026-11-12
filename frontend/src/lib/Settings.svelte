<script lang="ts">
  import Avatar from './Avatar.svelte';
  import { dragRegion } from './drag';
  import General from './General.svelte';
  import Icon from './Icon.svelte';
  import Permissions from './Permissions.svelte';
  import type { Emotion } from './avatar';
  import { t, type MessageKey } from './i18n';
  import type { IconName } from './icons';
  import Extensions from './settings/Extensions.svelte';
  import IslandPage from './settings/IslandPage.svelte';
  import Memory from './settings/Memory.svelte';
  import Persona from './settings/Persona.svelte';
  import Providers from './settings/Providers.svelte';
  import Tools from './settings/Tools.svelte';
  import Voice from './settings/Voice.svelte';

  type Page = 'persona' | 'providers' | 'permissions' | 'tools' | 'memory' | 'extensions' | 'voice' | 'island' | 'general';

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
    { id: 'general', icon: 'set', lead: 'general.lead' },
  ];

  // svelte-ignore state_referenced_locally
  let page = $state<Page>(start);
  const current = $derived(PAGES.find((p) => p.id === page) ?? PAGES[0]);
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onback()} />

<div class="app">
  <aside class="side" use:dragRegion>
    <div class="me">
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
    <h3>{t(`settings.page.${page}`)}</h3>
    <p class="lead">{t(current.lead)}</p>
    {#key page}
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
    background: #0d0e17;
  }
  .side {
    background: rgba(255, 255, 255, 0.025);
    border-right: 1px solid var(--line);
    padding: 14px 12px;
    min-height: 0;
    overflow: hidden;
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
    padding: 8px 12px;
    border-radius: 12px;
    font-size: 13.5px;
    font-weight: 600;
    color: var(--mute);
    margin-bottom: 2px;
    transition: color 0.2s;
  }
  nav button:hover:not(:disabled) {
    color: #fff;
  }
  nav button.on {
    color: #fff;
    background: rgba(143, 125, 255, 0.18);
  }
  .page {
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
