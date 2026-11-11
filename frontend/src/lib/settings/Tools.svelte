<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';

  let webEnabled = $state(false);
  let error = $state('');

  onMount(async () => {
    const ui = await invoke<{ web_enabled: boolean }>('get_ui_settings');
    webEnabled = ui.web_enabled;
  });

  async function toggleWeb(v: boolean) {
    try {
      await invoke('set_web_enabled', { value: v });
      webEnabled = v;
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

<Switch checked={webEnabled} title={t('tools.web')} sub={t('tools.web_sub')} onchange={toggleWeb} />
<p class="hint">{t('tools.later')}</p>
{#if error}<p class="err">{error}</p>{/if}

<style>
  .hint {
    margin: 14px 2px 0;
    font-size: 12.5px;
    color: var(--dim);
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
