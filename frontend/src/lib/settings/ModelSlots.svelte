<script lang="ts">
  // How the chosen model shows each of the ten emotions, and how wide its lips move.
  import { invoke } from '@tauri-apps/api/core';
  import { EMOTION_ORDER, type Emotion } from '../avatar';
  import { t } from '../i18n';
  import { model } from '../model/state.svelte';

  let { onshow }: { onshow: (emotion: Emotion) => void } = $props();

  let error = $state('');

  const profile = $derived(model.profile);

  /** The expression the user chose for the slot; empty when the program chooses. */
  function chosen(emotion: Emotion): string {
    const slot = profile?.slots[emotion];
    return slot?.source === 'manual' ? (slot.mix[0]?.[0] ?? '') : '';
  }

  async function run(command: string, args: Record<string, unknown>) {
    try {
      await invoke(command, args);
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  function pick(emotion: Emotion, expression: string) {
    onshow(emotion);
    run('avatar_model_set_slot', { id: model.id, emotion, expression: expression === '' ? null : expression });
  }
</script>

{#if profile}
  <div class="lbl">{t('persona.slots')}</div>
  <p class="lead">{t('persona.slots_lead')}</p>
  <div class="slots">
    {#each EMOTION_ORDER as e (e)}
      <label>
        <span>{t(`emotion.${e}`)}</span>
        <select value={chosen(e)} onfocus={() => onshow(e)} onchange={(ev) => pick(e, ev.currentTarget.value)}>
          <option value="">{t('persona.slot_auto')}</option>
          {#each profile.expressions as x (x)}<option value={x}>{x}</option>{/each}
        </select>
      </label>
    {/each}
  </div>

  <div class="lbl">{t('persona.lip')}</div>
  <input
    class="lip"
    type="range"
    min="0"
    max="1.5"
    step="0.1"
    value={profile.lip}
    aria-label={t('persona.lip')}
    onchange={(ev) => run('avatar_model_set_lip', { id: model.id, lip: Number(ev.currentTarget.value) })}
  />
  {#if error}<p class="err">{error}</p>{/if}
{/if}

<style>
  .lead {
    margin: -2px 0 8px;
    font-size: 12px;
    color: var(--mute);
  }
  .slots {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px 14px;
  }
  label {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--mute);
  }
  label span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  select {
    box-sizing: border-box;
    min-width: 0;
    width: 100%;
    height: 30px;
    padding: 0 8px;
    border-radius: 9px;
    border: 1px solid var(--line2);
    background: #14131f;
    color: #fff;
    font: inherit;
    font-size: 12.5px;
  }
  select:focus {
    outline: none;
    border-color: rgba(143, 125, 255, 0.6);
  }
  .lip {
    width: 100%;
    max-width: 260px;
    accent-color: var(--v);
  }
  .err {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
