<script lang="ts">
  // Vision and picture models: they use the LLM address and key unless given their own.
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { t } from '../i18n';

  interface MediaProviders {
    vision_model: string;
    image_base_url: string;
    image_model: string;
    has_image_key: boolean;
  }

  let vision = $state('');
  let imageUrl = $state('');
  let imageModel = $state('');
  let imageKey = $state('');
  let hasKey = $state(false);
  let status = $state('');
  let statusKind = $state<'ok' | 'bad'>('ok');

  onMount(async () => {
    const m = await invoke<MediaProviders>('get_media_providers');
    vision = m.vision_model;
    imageUrl = m.image_base_url;
    imageModel = m.image_model;
    hasKey = m.has_image_key;
  });

  async function save() {
    try {
      await invoke('save_media_providers', {
        visionModel: vision,
        imageBaseUrl: imageUrl,
        imageModel,
        imageKey: imageKey ? imageKey : null,
      });
      if (imageKey) hasKey = true;
      imageKey = '';
      status = t('providers.saved');
      statusKind = 'ok';
    } catch (e) {
      status = String(e);
      statusKind = 'bad';
    }
  }
</script>

<div class="prov">
  <div class="ico"><Icon name="mon" size={19} /></div>
  <div class="what"><b>{t('providers.vision')}</b><small>{t('providers.vision_sub')}</small></div>
  <span class="lat">{vision.trim() || t('providers.same_model')}</span>
</div>
<div class="prov">
  <div class="ico"><Icon name="star" size={19} /></div>
  <div class="what"><b>{t('providers.image')}</b><small>{t('providers.image_sub')}</small></div>
  <span class="lat" class:ok={!!imageModel.trim()}>{imageModel.trim() || t('providers.off')}</span>
</div>

<div class="form">
  <label>
    <span class="lbl">{t('providers.vision_model')}</span>
    <input class="fld" bind:value={vision} spellcheck="false" />
  </label>
  <label>
    <span class="lbl">{t('providers.image_model')}</span>
    <input class="fld" bind:value={imageModel} spellcheck="false" />
  </label>
  <label>
    <span class="lbl">{t('providers.image_url')}</span>
    <input class="fld" bind:value={imageUrl} spellcheck="false" />
  </label>
  <label>
    <span class="lbl">{t('providers.image_key')}</span>
    <input class="fld" type="password" bind:value={imageKey} placeholder={hasKey ? t('providers.key_saved') : ''} autocomplete="off" />
  </label>
  <div class="row">
    <button class="bt pri" onclick={save}>{t('providers.save')}</button>
    {#if status}<span class="status {statusKind}">{status}</span>{/if}
  </div>
</div>

<style>
  .prov {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px 16px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    margin-bottom: 10px;
  }
  .ico {
    flex: none;
    width: 40px;
    height: 40px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: rgba(143, 125, 255, 0.16);
    color: var(--v);
  }
  .what {
    min-width: 0;
  }
  .what b {
    display: block;
    font-size: 14px;
  }
  .what small {
    display: block;
    color: var(--mute);
    font-size: 12.5px;
  }
  .lat {
    margin-left: auto;
    flex: none;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 12px;
    font-weight: 700;
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
    padding: 5px 10px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .lat.ok {
    color: var(--ok);
    background: rgba(95, 227, 161, 0.1);
  }
  .form {
    margin: 0 0 14px;
    padding: 4px 16px 16px;
    border-radius: 16px;
    border: 1px solid var(--line);
  }
  label {
    display: block;
  }
  label .lbl {
    display: block;
    margin: 12px 0 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
  }
  .status {
    font-size: 12.5px;
    user-select: text;
  }
  .status.ok {
    color: var(--ok);
  }
  .status.bad {
    color: var(--bad);
  }
</style>
