<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Avatar from '../Avatar.svelte';
  import { EMOTION_ORDER, type Emotion } from '../avatar';
  import { t } from '../i18n';
  import ModelSlots from './ModelSlots.svelte';
  import ModelTiles from './ModelTiles.svelte';

  const PERSONAS = ['caring', 'cheerful', 'calm', 'bold', 'custom'] as const;
  type Persona = (typeof PERSONAS)[number];

  interface UiSettings {
    name: string;
    persona: Persona;
    custom_prompt: string;
    avatar_in_island: boolean;
    avatar_on_desktop: boolean;
  }

  let name = $state('');
  let savedName = $state('');
  let persona = $state<Persona>('caring');
  let customPrompt = $state('');
  let inIsland = $state(true);
  let onDesktop = $state(false);
  let shown = $state<Emotion>('neutral');
  let error = $state('');

  onMount(async () => {
    const s = await invoke<UiSettings>('get_ui_settings');
    name = savedName = s.name;
    persona = s.persona;
    customPrompt = s.custom_prompt;
    inIsland = s.avatar_in_island;
    onDesktop = s.avatar_on_desktop;
  });

  async function run(command: string, args: Record<string, unknown>): Promise<boolean> {
    try {
      await invoke(command, args);
      error = '';
      return true;
    } catch (e) {
      error = String(e);
      return false;
    }
  }

  async function saveName() {
    if (name.trim() === savedName) return;
    if (await run('set_assistant_name', { value: name })) {
      name = savedName = name.trim();
    } else {
      name = savedName;
    }
  }

  function savePersona(next: Persona) {
    persona = next;
    run('set_persona', { persona, customPrompt });
  }

  function place(island: boolean, desktop: boolean) {
    inIsland = island;
    onDesktop = desktop;
    run('set_avatar_placement', { inIsland, onDesktop });
  }
</script>

<div class="pers">
  <div class="left">
    <div class="prev">
      <div class="halo"></div>
      <div class="girlbox"><Avatar view="full" emotion={shown} /></div>
      <div class="plate"><b>{t(`emotion.${shown}`)}</b><span>{t(`emotion.why.${shown}`)}</span></div>
    </div>
    <div class="lbl">{t('persona.emotions')}</div>
    <div class="emos">
      {#each EMOTION_ORDER as e (e)}
        <button class:on={shown === e} title={t(`emotion.${e}`)} aria-label={t(`emotion.${e}`)} onclick={() => (shown = e)}>
          <Avatar view="head" emotion={e} size={34} />
        </button>
      {/each}
    </div>
  </div>

  <div class="right">
    <div class="lbl">{t('persona.name')}</div>
    <input
      class="fld"
      bind:value={name}
      maxlength="24"
      spellcheck="false"
      aria-label={t('persona.name')}
      onblur={saveName}
      onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
    />

    <div class="lbl">{t('persona.model')}</div>
    <ModelTiles name={savedName} />
    <ModelSlots onshow={(e) => (shown = e)} />

    <div class="lbl">{t('persona.character')}</div>
    <div class="chips" role="radiogroup" aria-label={t('persona.character')}>
      {#each PERSONAS as p (p)}
        <button role="radio" aria-checked={persona === p} class:on={persona === p} onclick={() => savePersona(p)}>
          {t(`persona.${p}`)}
        </button>
      {/each}
    </div>
    {#if persona === 'custom'}
      <textarea
        class="fld custom"
        rows="3"
        maxlength="1000"
        bind:value={customPrompt}
        placeholder={t('persona.custom_placeholder')}
        aria-label={t('persona.custom')}
        onblur={() => savePersona('custom')}
      ></textarea>
    {/if}

    <div class="lbl">{t('persona.where')}</div>
    <div class="chips">
      <button aria-pressed={inIsland} class:on={inIsland} onclick={() => place(!inIsland, onDesktop)}>{t('persona.in_island')}</button>
      <button aria-pressed={onDesktop} class:on={onDesktop} onclick={() => place(inIsland, !onDesktop)}>
        {t('persona.on_desktop')}
      </button>
    </div>
    {#if error}<p class="err">{error}</p>{/if}
  </div>
</div>

<style>
  .pers {
    display: grid;
    grid-template-columns: 206px 1fr;
    gap: 22px;
  }
  /* The preview stays in sight while the long right column scrolls: a slot chosen there is shown here. */
  .left {
    position: sticky;
    top: 0;
    align-self: start;
  }
  .prev {
    position: relative;
    height: 236px;
    margin-bottom: 14px;
    border-radius: 20px;
    overflow: hidden;
    background: linear-gradient(180deg, #191640, #0b0b16);
    border: 1px solid var(--line);
  }
  .prev :global(.halo) {
    width: 220px;
    height: 220px;
  }
  .girlbox {
    position: absolute;
    left: 50%;
    bottom: 26px;
    width: 176px;
    translate: -50% 0;
  }
  .plate {
    position: absolute;
    left: 8px;
    right: 8px;
    bottom: 8px;
    padding: 6px 10px;
    border-radius: 12px;
    background: rgba(8, 9, 15, 0.66);
    backdrop-filter: blur(14px);
    border: 1px solid var(--line);
    font-size: 12px;
    display: flex;
    justify-content: space-between;
    gap: 8px;
    white-space: nowrap;
  }
  .plate span {
    color: var(--mute);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .emos {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: 6px;
  }
  .emos button {
    all: unset;
    cursor: pointer;
    display: grid;
    place-items: center;
    aspect-ratio: 1;
    border-radius: 50%;
    border: 1.5px solid transparent;
    transition: border-color 0.2s;
  }
  .emos button:hover,
  .emos button.on {
    border-color: var(--v);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .chips button {
    all: unset;
    cursor: pointer;
    padding: 7px 13px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 600;
    border: 1px solid var(--line2);
    color: var(--mute);
    transition: 0.2s;
  }
  .chips button:hover {
    color: #fff;
  }
  .chips button.on {
    color: #fff;
    border-color: transparent;
    background: linear-gradient(135deg, rgba(143, 125, 255, 0.55), rgba(255, 126, 182, 0.4));
  }
  .custom {
    margin-top: 10px;
    font-size: 13px;
  }
  .err {
    margin: 12px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
