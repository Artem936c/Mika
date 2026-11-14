<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import EveningSummary from './EveningSummary.svelte';
  import Rhythm from './Rhythm.svelte';
  import Research from './Research.svelte';
  import { isMessageKey, t, type MessageKey } from '../i18n';

  interface ToolSettings {
    web_enabled: boolean;
    screen_enabled: boolean;
    news_feed: string;
    groups: { id: string; enabled: boolean }[];
    tool_count: number;
    notifications: { enabled: boolean; only_messengers: boolean; read_aloud: boolean };
    briefing_enabled: boolean;
    briefing_time: string;
  }

  interface FfmpegView {
    found: boolean;
    source: 'settings' | 'path' | 'downloaded' | null;
    path: string;
    version: string;
    gpl: boolean;
    setting: string;
    setting_invalid: boolean;
    downloaded: boolean;
    downloading: boolean;
    bytes: number;
    download_mb: number;
  }

  let s = $state<ToolSettings | null>(null);
  let feed = $state('');
  let briefingTime = $state('08:00');
  let note = $state('');
  let error = $state('');

  let ff = $state<FfmpegView | null>(null);
  let ffPath = $state('');
  let ffShare = $state(0);
  let ffError = $state('');
  let ffNote = $state('');

  onMount(() => {
    load();
    loadFfmpeg();
    const off = listen<number>('ffmpeg-progress', (e) => (ffShare = e.payload));
    return () => {
      off.then((f) => f());
    };
  });

  async function loadFfmpeg() {
    try {
      ff = await invoke<FfmpegView>('get_ffmpeg_status');
      ffPath = ff.setting;
    } catch (e) {
      ffError = String(e);
    }
  }

  async function ffCall(command: string, args: Record<string, unknown> = {}) {
    try {
      ff = await invoke<FfmpegView>(command, args);
      ffPath = ff.setting;
      ffError = '';
      return true;
    } catch (e) {
      ffError = String(e);
      await loadFfmpeg();
      return false;
    }
  }

  async function saveFfmpegPath() {
    if (await ffCall('set_ffmpeg_path', { value: ffPath })) ffNote = t('island.saved');
  }

  async function getFfmpeg() {
    if (!ff) return;
    ff.downloading = true;
    ffShare = 0;
    ffError = '';
    await ffCall('install_ffmpeg');
  }

  const ffSource: Record<NonNullable<FfmpegView['source']>, MessageKey> = {
    settings: 'tools.ffmpeg_source_settings',
    path: 'tools.ffmpeg_source_path',
    downloaded: 'tools.ffmpeg_source_downloaded',
  };
  const ffSize = $derived(ff ? Math.round(ff.bytes / (1024 * 1024)) : 0);

  async function load() {
    try {
      s = await invoke<ToolSettings>('get_tool_settings');
      feed = s.news_feed;
      briefingTime = s.briefing_time;
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

  function setNotify(change: Partial<ToolSettings['notifications']>) {
    if (!s) return;
    const n = { ...s.notifications, ...change };
    // Tauri takes command arguments in camelCase.
    run('set_notifications', { enabled: n.enabled, onlyMessengers: n.only_messengers, readAloud: n.read_aloud });
  }

  function setBriefing(enabled: boolean) {
    run('set_briefing', { enabled, time: briefingTime });
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

  <div class="lbl">{t('tools.notify')}</div>
  <div class="stack">
    <Switch
      checked={s.notifications.enabled}
      title={t('tools.notify_on')}
      sub={t('tools.notify_on_sub')}
      onchange={(v) => setNotify({ enabled: v })}
    />
    {#if s.notifications.enabled}
      <Switch
        checked={s.notifications.only_messengers}
        title={t('tools.notify_messengers')}
        sub={t('tools.notify_messengers_sub')}
        onchange={(v) => setNotify({ only_messengers: v })}
      />
      <Switch
        checked={s.notifications.read_aloud}
        title={t('tools.notify_aloud')}
        sub={t('tools.notify_aloud_sub')}
        onchange={(v) => setNotify({ read_aloud: v })}
      />
    {/if}
  </div>

  <div class="lbl">{t('tools.briefing')}</div>
  <div class="stack">
    <Switch checked={s.briefing_enabled} title={t('tools.briefing_on')} sub={t('tools.briefing_sub')} onchange={setBriefing} />
  </div>
  <div class="line time">
    <span>{t('tools.briefing_time')}</span>
    <input class="fld" type="time" bind:value={briefingTime} onchange={() => s && setBriefing(s.briefing_enabled)} />
  </div>
  <EveningSummary />

  <Rhythm />

  <div class="lbl">{t('tools.feed')}</div>
  <div class="line">
    <input class="fld" bind:value={feed} spellcheck="false" oninput={() => (note = '')} />
    <button class="bt" onclick={saveFeed}>{t('tools.feed_save')}</button>
  </div>
  {#if note}<p class="ok">{note}</p>{/if}

  <Research />

  <div class="lbl">{t('tools.ffmpeg')}</div>
  <p class="hint">{t('tools.ffmpeg_sub')}</p>
  {#if ff}
    {#if ff.downloading}
      <p class="hint">{t('tools.ffmpeg_loading', { share: ffShare })}</p>
      <div class="bar"><i style:width="{ffShare}%"></i></div>
    {:else if ff.found}
      <p class="found">
        {t('tools.ffmpeg_found', { version: ff.version || '?', path: ff.path })}{#if ff.source}&nbsp;({t(ffSource[ff.source])}){/if}
      </p>
      {#if ff.gpl}<p class="hint">{t('tools.ffmpeg_gpl')}</p>{/if}
    {:else}
      <p class="hint">{t('tools.ffmpeg_missing')} {t('tools.ffmpeg_offer', { size: ff.download_mb })}</p>
      <div class="acts">
        <button class="bt pri" onclick={getFfmpeg}>{t('tools.ffmpeg_get', { size: ff.download_mb })}</button>
      </div>
    {/if}
    {#if ff.downloaded && !ff.downloading}
      <p class="hint">{t('tools.ffmpeg_on', { size: ffSize })}</p>
      <div class="acts">
        <button class="bt" onclick={() => ffCall('remove_ffmpeg')}>{t('tools.ffmpeg_remove')}</button>
      </div>
      <p class="hint">{t('tools.ffmpeg_license')}</p>
    {/if}
    <div class="sub">{t('tools.ffmpeg_path')}</div>
    <div class="line">
      <input
        class="fld"
        bind:value={ffPath}
        spellcheck="false"
        placeholder={t('tools.ffmpeg_path_hint')}
        oninput={() => (ffNote = '')}
        onkeydown={(e) => e.key === 'Enter' && saveFfmpegPath()}
      />
      <button class="bt" onclick={saveFfmpegPath}>{t('tools.ffmpeg_path_save')}</button>
    </div>
    {#if ff.setting_invalid}<p class="err">{t('tools.ffmpeg_path_bad')}</p>{/if}
    {#if ffNote}<p class="ok">{ffNote}</p>{/if}
  {/if}
  {#if ffError}<p class="err">{ffError}</p>{/if}

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
  .time {
    align-items: center;
    margin: 10px 2px 4px;
    font-size: 13px;
  }
  .time .fld {
    width: 120px;
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
  .found {
    margin: 0 2px 6px;
    font-size: 13px;
    word-break: break-all;
    user-select: text;
  }
  .acts {
    display: flex;
    gap: 8px;
    margin: 0 0 10px;
  }
  .sub {
    margin: 12px 2px 6px;
    font-size: 12.5px;
    color: var(--mute);
  }
  .bar {
    height: 4px;
    margin: 0 2px 10px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--c));
    transition: width 0.3s;
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
