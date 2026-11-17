<script lang="ts">
  // How the chosen model stands and moves: rest pose, size on the desktop,
  // frame in the chat, the cursor and idle actions (decision 0040).
  import { invoke } from '@tauri-apps/api/core';
  import { t } from '../i18n';
  import { model, type Look } from '../model/state.svelte';

  const POSES = ['relaxed', 'clasped', 'behind', 'hip', 'crossed', 'auto'] as const;
  const FRAMES = ['full', 'waist', 'close'] as const;

  let error = $state('');

  async function save(change: Partial<Look>) {
    try {
      await invoke('avatar_set_look', { look: { ...model.look, ...change } });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

{#if model.id}
  <div class="lbl">{t('persona.look_pose')}</div>
  <div class="chips" role="radiogroup" aria-label={t('persona.look_pose')}>
    {#each POSES as p (p)}
      <button class="tg" role="radio" aria-checked={model.look.pose === p} class:on={model.look.pose === p} onclick={() => save({ pose: p })}>
        {t(`persona.pose_${p}`)}
      </button>
    {/each}
  </div>

  <div class="lbl">{t('persona.look_frame')}</div>
  <div class="chips" role="radiogroup" aria-label={t('persona.look_frame')}>
    {#each FRAMES as f (f)}
      <button class="tg" role="radio" aria-checked={model.look.frame === f} class:on={model.look.frame === f} onclick={() => save({ frame: f })}>
        {t(`persona.frame_${f}`)}
      </button>
    {/each}
  </div>

  <div class="lbl">{t('persona.look_scale')}</div>
  <div class="scale">
    <input
      type="range"
      min="0.6"
      max="1.6"
      step="0.1"
      value={model.look.scale}
      aria-label={t('persona.look_scale')}
      onchange={(ev) => save({ scale: Number(ev.currentTarget.value) })}
    />
    <span>{Math.round(model.look.scale * 100)}%</span>
  </div>
  <p class="lead">{t('persona.look_scale_lead')}</p>

  <div class="chips">
    <button class="tg" aria-pressed={model.look.follow_cursor} class:on={model.look.follow_cursor} onclick={() => save({ follow_cursor: !model.look.follow_cursor })}>
      {t('persona.look_cursor')}
    </button>
    <button class="tg" aria-pressed={model.look.idle_actions} class:on={model.look.idle_actions} onclick={() => save({ idle_actions: !model.look.idle_actions })}>
      {t('persona.look_idle')}
    </button>
  </div>
  {#if error}<p class="err">{error}</p>{/if}
{/if}

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 6px;
  }
  .scale {
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: 300px;
  }
  .scale input {
    flex: 1;
    accent-color: var(--v);
  }
  .scale span {
    min-width: 40px;
    font-size: 12.5px;
    color: var(--mute);
    text-align: right;
  }
  .lead {
    margin: 4px 0 8px;
    font-size: 12px;
    color: var(--mute);
  }
  .err {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
