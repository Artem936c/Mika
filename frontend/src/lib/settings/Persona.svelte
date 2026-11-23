<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Avatar from '../Avatar.svelte';
  import { EMOTION_ORDER, type Emotion } from '../avatar';
  import { t } from '../i18n';
  import { personaLabel, type PersonaView } from '../persona';
  import Switch from '../Switch.svelte';
  import ModelLook from './ModelLook.svelte';
  import ModelMotions from './ModelMotions.svelte';
  import ModelSlots from './ModelSlots.svelte';
  import ModelTiles from './ModelTiles.svelte';

  interface UiSettings {
    name: string;
    avatar_in_island: boolean;
    avatar_on_desktop: boolean;
  }

  /** An own character open for editing: `old` is its saved name, null for a new one. */
  interface Draft {
    old: string | null;
    name: string;
    prompt: string;
  }

  let name = $state('');
  let savedName = $state('');
  let view = $state<PersonaView | null>(null);
  let draft = $state<Draft | null>(null);
  /** Why the open character was not saved: shown in its editor. */
  let draftError = $state('');
  /** The own character whose deletion waits for a second click. */
  let deleting = $state<string | null>(null);
  /** The question about the age (18+) is on screen. */
  let askingAge = $state(false);
  let inIsland = $state(true);
  let onDesktop = $state(false);
  let shown = $state<Emotion>('neutral');
  let error = $state('');

  onMount(async () => {
    const s = await invoke<UiSettings>('get_ui_settings');
    name = savedName = s.name;
    view = await invoke<PersonaView>('get_personas');
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

  /** Runs a command of the character part and draws what it answers. */
  async function change(command: string, args: Record<string, unknown>): Promise<boolean> {
    try {
      view = await invoke<PersonaView>(command, args);
      error = '';
      return true;
    } catch (e) {
      error = String(e);
      return false;
    }
  }

  function choose(choice: string) {
    if (view?.choice === choice) return;
    change('set_persona', { choice });
  }

  function edit(old: string | null) {
    const own = view?.custom.find((c) => c.name === old);
    draft = { old, name: own?.name ?? '', prompt: own?.prompt ?? '' };
    draftError = '';
    deleting = null;
  }

  async function saveDraft() {
    if (!draft) return;
    if (await change('save_custom_persona', { old: draft.old, name: draft.name, prompt: draft.prompt })) {
      draft = null;
    } else {
      draftError = error;
      error = '';
    }
  }

  async function remove(own: string) {
    if (deleting !== own) {
      deleting = own;
      return;
    }
    deleting = null;
    if (draft?.old === own) draft = null;
    await change('delete_custom_persona', { name: own });
  }

  /** Turning on asks the age first; turning off needs no question. */
  function toggleAdult(on: boolean) {
    if (on && !view?.adult) {
      askingAge = true;
      return;
    }
    askingAge = false;
    if (!on && view?.adult) change('set_adult', { on: false });
  }

  async function answerAge(adult: boolean) {
    askingAge = false;
    if (adult) await change('set_adult', { on: true });
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
    <ModelLook />
    <ModelSlots onshow={(e) => (shown = e)} />
    <ModelMotions />

    {#if view}
      {@const v = view}
      <div class="lbl">{t('persona.character')}</div>
      <div class="chips" role="radiogroup" aria-label={t('persona.character')}>
        {#each [...v.ready, ...v.custom.map((c) => c.name)] as p (p)}
          <button role="radio" aria-checked={v.choice === p} class:on={v.choice === p} class:own={!v.ready.includes(p)} onclick={() => choose(p)}>
            {personaLabel(p, v.ready)}
          </button>
        {/each}
      </div>

      <div class="lbl head">
        <span>{t('persona.own')}</span>
        <span class="count">{t('persona.own_count', { n: v.custom.length, max: v.max_custom })}</span>
      </div>
      <p class="sub">{t('persona.own_lead', { max: v.max_custom })}</p>
      {#snippet editor(d: Draft)}
        <div class="editor">
          <input
            class="fld"
            bind:value={d.name}
            maxlength={v.max_name}
            spellcheck="false"
            placeholder={t('persona.own_name')}
            aria-label={t('persona.own_name')}
          />
          <textarea class="fld" rows="4" maxlength={v.max_prompt} bind:value={d.prompt} placeholder={t('persona.own_prompt')} aria-label={t('persona.own_prompt')}
          ></textarea>
          <div class="acts">
            <span class="chars">{t('persona.own_chars', { n: d.prompt.length, max: v.max_prompt })}</span>
            <button class="bt" onclick={() => (draft = null)}>{t('persona.own_cancel')}</button>
            <button class="bt pri" disabled={!d.name.trim() || !d.prompt.trim()} onclick={saveDraft}>{t('persona.own_save')}</button>
          </div>
          {#if draftError}<p class="err">{draftError}</p>{/if}
        </div>
      {/snippet}
      <div class="own">
        {#each v.custom as c (c.name)}
          {#if draft && draft.old === c.name}
            {@render editor(draft)}
          {:else}
            <div class="row">
              <div class="txt">
                <b>{c.name}</b>
                <span>{c.prompt}</span>
              </div>
              <button class="bt" onclick={() => edit(c.name)}>{t('persona.own_edit')}</button>
              <button class="bt no" onclick={() => remove(c.name)}>
                {deleting === c.name ? t('persona.own_delete_confirm', { name: c.name }) : t('persona.own_delete')}
              </button>
            </div>
          {/if}
        {/each}
        {#if draft && draft.old === null}
          {@render editor(draft)}
        {:else if v.custom.length < v.max_custom}
          <button class="add" onclick={() => edit(null)}>{t('persona.own_add')}</button>
        {/if}
      </div>

      <div class="adult"></div>
      <Switch checked={v.adult || askingAge} title={t('persona.adult')} sub={t('persona.adult_lead')} onchange={toggleAdult} />
      {#if askingAge}
        <div class="ask" role="alertdialog" aria-label={t('persona.adult_ask')}>
          <b>{t('persona.adult_ask')}</b>
          <span class="grow"></span>
          <button class="bt" onclick={() => answerAge(false)}>{t('persona.adult_no')}</button>
          <button class="bt pri" onclick={() => answerAge(true)}>{t('persona.adult_yes')}</button>
        </div>
      {/if}
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
    background: linear-gradient(180deg, var(--deep-c), var(--deep-b));
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
    background: var(--veil);
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
    background: linear-gradient(135deg, rgba(var(--v-rgb), 0.55), rgba(var(--p-rgb), 0.4));
  }
  /* A long own text is cut with an ellipsis; without this the column would grow to fit it. */
  .right {
    min-width: 0;
  }
  .chips button.own {
    border-style: dashed;
  }
  .chips button.own.on {
    border-style: solid;
  }
  .lbl.head {
    display: flex;
    justify-content: space-between;
    gap: 10px;
  }
  .count {
    letter-spacing: 0;
    text-transform: none;
    font-weight: 600;
  }
  .sub {
    margin: -2px 0 10px;
    font-size: 12.5px;
    color: var(--mute);
  }
  .own {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 10px 10px 14px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
  }
  .row .txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
  }
  .row .txt span {
    color: var(--mute);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row .bt {
    height: 30px;
    padding: 0 11px;
    font-size: 12px;
  }
  .editor {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 14px;
    border: 1px solid rgba(var(--v-rgb), 0.35);
    background: rgba(var(--v-rgb), 0.06);
  }
  .editor textarea {
    font-size: 13px;
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .chars {
    flex: 1;
    font-size: 11.5px;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
  }
  .add {
    all: unset;
    cursor: pointer;
    align-self: flex-start;
    padding: 8px 14px;
    border-radius: 12px;
    border: 1px dashed var(--line2);
    color: var(--mute);
    font-size: 13px;
    font-weight: 700;
  }
  .add:hover {
    color: var(--v);
    border-color: rgba(var(--v-rgb), 0.5);
  }
  .adult {
    height: 18px;
  }
  .ask {
    margin-top: 8px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 10px 10px 16px;
    border-radius: 14px;
    border: 1px solid rgba(var(--p-rgb), 0.4);
    background: rgba(var(--p-rgb), 0.1);
    font-size: 14px;
  }
  .grow {
    flex: 1;
  }
  .editor .err {
    margin: 0;
  }
  .err {
    margin: 12px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
