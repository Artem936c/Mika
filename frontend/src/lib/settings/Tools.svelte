<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { isMessageKey, t } from '../i18n';

  interface ToolSettings {
    web_enabled: boolean;
    screen_enabled: boolean;
    news_feed: string;
    groups: { id: string; enabled: boolean }[];
    tool_count: number;
  }

  let s = $state<ToolSettings | null>(null);
  let feed = $state('');
  let note = $state('');
  let error = $state('');

  onMount(load);

  async function load() {
    try {
      s = await invoke<ToolSettings>('get_tool_settings');
      feed = s.news_feed;
    } catch (e) {
      error = String(e);
    }
  }

  async function run(command: string, args: Record<string, unknown>) {
    try {
      await invoke(command, args);
      error = '';
      await load();
    } catch (e) {
      error = String(e);
    }
  }

  async function saveFeed() {
    await run('set_news_feed', { value: feed });
    if (!error) note = t('island.saved');
  }

  const groupTitle = (id: string) => {
    const key = `group.${id}`;
    return isMessageKey(key) ? t(key) : id;
  };
</script>

{#if s}
  <div class="stack">
    <Switch checked={s.web_enabled} title={t('tools.web')} sub={t('tools.web_sub')} onchange={(v) => run('set_web_enabled', { value: v })} />
    <Switch
      checked={s.screen_enabled}
      title={t('tools.screen')}
      sub={t('tools.screen_sub')}
      onchange={(v) => run('set_screen_enabled', { value: v })}
    />
  </div>

  <div class="lbl">{t('tools.feed')}</div>
  <div class="line">
    <input class="fld" bind:value={feed} spellcheck="false" oninput={() => (note = '')} />
    <button class="bt" onclick={saveFeed}>{t('tools.feed_save')}</button>
  </div>
  {#if note}<p class="ok">{note}</p>{/if}

  <div class="lbl">{t('tools.groups')}</div>
  <p class="hint">{t('tools.groups_sub')} {t('tools.count', { count: s.tool_count })}</p>
  <div class="groups">
    {#each s.groups as g (g.id)}
      <label class="grp">
        <input type="checkbox" checked={g.enabled} onchange={(e) => run('set_tool_group', { group: g.id, enabled: e.currentTarget.checked })} />
        <span>{groupTitle(g.id)}</span>
      </label>
    {/each}
  </div>
  <p class="hint">{t('tools.later')}</p>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .stack {
    display: grid;
    gap: 10px;
  }
  .line {
    display: flex;
    gap: 8px;
  }
  .hint {
    margin: 0 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .groups {
    display: grid;
    gap: 2px;
    margin-bottom: 14px;
  }
  .grp {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 10px;
    font-size: 13px;
    cursor: pointer;
  }
  .grp:hover {
    background: rgba(255, 255, 255, 0.04);
  }
  .grp input {
    margin-top: 2px;
    accent-color: var(--v);
  }
  .ok {
    margin: 8px 2px 0;
    font-size: 12.5px;
    color: var(--ok);
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
