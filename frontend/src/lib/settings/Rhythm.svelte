<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';
  import {
    calendarArgs,
    companionArgs,
    companionFlip,
    linkAfterSave,
    rhythmArgs,
    uniqueFolders,
    withFolder,
    withoutFolder,
    type Companion,
    type Rhythm,
  } from '../rhythm';

  interface DocsStatus {
    files: number;
    chunks: number;
    embedded: number;
    running: boolean;
    last_run: number | null;
    error: string | null;
    progress: [number, number] | null;
  }

  interface RhythmSettings {
    rhythm: Rhythm;
    companion: Companion;
    screen_enabled: boolean;
    tracking: { app_time: boolean; clipboard: boolean };
    clip_count: number;
    focus_left_s: number | null;
    calendar_enabled: boolean;
    remind_min: number;
    has_calendar_url: boolean;
    calendar_events: number;
    calendar_read_at: string | null;
    calendar_error: string | null;
    docs_folders: string[];
    docs: DocsStatus;
  }

  let s = $state<RhythmSettings | null>(null);
  let every = $state(50);
  let companionEvery = $state(60);
  // Turning the companion on asks first (decision 0054); without the Screen switch the page only explains.
  let askingCompanion = $state(false);
  let companionNote = $state('');
  // Redraws the switch after a refused turn on: the box itself was ticked by the click.
  let companionKey = $state(0);
  let remind = $state(10);
  let calendarUrl = $state('');
  let newFolder = $state('');
  let error = $state('');
  let timer: ReturnType<typeof setInterval> | undefined;

  // A folder written twice in config.toml must not break the keyed list.
  const folders = $derived(s ? uniqueFolders(s.docs_folders) : []);

  onMount(() => {
    load();
    // Focus time and the reading of documents move while the page is open.
    timer = setInterval(() => load(), 3000);
  });
  onDestroy(() => clearInterval(timer));

  /** `sync`: the number fields take the saved values. Not on each poll: the user may be typing. */
  async function load(sync = false) {
    try {
      const fresh = await invoke<RhythmSettings>('get_rhythm_settings');
      if (!s || sync) {
        every = fresh.rhythm.break_every_min;
        companionEvery = fresh.companion.every_s;
        remind = fresh.remind_min;
      }
      s = fresh;
    } catch (e) {
      error = String(e);
    }
  }

  /** True when the command went through. */
  async function run(command: string, args: Record<string, unknown> = {}): Promise<boolean> {
    try {
      await invoke(command, args);
    } catch (e) {
      error = String(e);
      return false;
    }
    error = '';
    // The core clamps the minutes: the fields show what it kept.
    await load(true);
    return true;
  }

  function setRhythm(change: Partial<Rhythm>) {
    if (!s) return;
    run('set_rhythm', rhythmArgs(s.rhythm, every, change));
  }

  function setCompanion(enabled?: boolean) {
    if (!s) return;
    run('set_companion', companionArgs(s.companion, companionEvery, enabled));
  }

  function flipCompanion(on: boolean) {
    if (!s) return;
    companionNote = '';
    const step = companionFlip(on, s.companion.enabled, s.screen_enabled);
    if (step === 'ask') {
      askingCompanion = true;
      return;
    }
    askingCompanion = false;
    if (step === 'needs_screen') {
      companionNote = t('tools.companion_needs_screen');
      companionKey++;
    } else if (step === 'off') {
      setCompanion(false);
    }
  }

  function answerCompanion(yes: boolean) {
    askingCompanion = false;
    if (yes) setCompanion(true);
    else companionKey++;
  }

  // Tauri takes command arguments in camelCase.
  function setTracking(change: Partial<RhythmSettings['tracking']>) {
    if (!s) return;
    const r = { ...s.tracking, ...change };
    run('set_tracking', { appTime: r.app_time, clipboard: r.clipboard });
  }

  async function saveCalendar(enabled: boolean) {
    if (!s) return;
    const sent = calendarUrl;
    const saved = await run('save_calendar', calendarArgs(enabled, remind, s.remind_min, sent));
    calendarUrl = linkAfterSave(saved, sent, calendarUrl);
  }

  // One change of the folders at a time: each sends the whole list, and a
  // network folder may take a while to answer.
  let savingFolders = $state(false);

  async function setFolders(next: string[]) {
    if (savingFolders) return;
    savingFolders = true;
    try {
      await run('set_docs_folders', { folders: uniqueFolders(next) });
    } finally {
      savingFolders = false;
    }
  }

  function addFolder() {
    if (!s || savingFolders) return;
    const next = withFolder(s.docs_folders, newFolder);
    if (next) setFolders(next);
    newFolder = '';
  }

  const minutes = (sec: number) => Math.max(1, Math.ceil(sec / 60));
</script>

{#if s}
  <div class="lbl">{t('tools.rhythm')}</div>
  <div class="stack">
    <div class="card">
      <div class="head">
        <div>
          <div class="ttl">{t('tools.focus')}</div>
          <div class="sub">{t('tools.focus_sub')}</div>
        </div>
      </div>
      <div class="line">
        {#if s.focus_left_s}
          <span class="state">{t('tools.focus_left', { min: minutes(s.focus_left_s) })}</span>
          <button class="bt" onclick={() => run('stop_focus')}>{t('tools.focus_stop')}</button>
        {:else}
          {#each [25, 50, 90] as m (m)}
            <button class="bt" onclick={() => run('start_focus', { minutes: m })}>{t('tools.focus_start', { min: m })}</button>
          {/each}
        {/if}
      </div>
    </div>
    <Switch checked={s.rhythm.breaks} title={t('tools.breaks')} sub={t('tools.breaks_sub')} onchange={(v) => setRhythm({ breaks: v })} />
    {#if s.rhythm.breaks}
      <div class="line time">
        <span>{t('tools.breaks_every')}</span>
        <input class="fld" type="number" min="15" max="240" step="5" bind:value={every} onchange={() => setRhythm({})} />
      </div>
    {/if}
    <Switch checked={s.rhythm.lively} title={t('tools.lively')} sub={t('tools.lively_sub')} onchange={(v) => setRhythm({ lively: v })} />
    {#key companionKey}
      <Switch
        checked={s.companion.enabled || askingCompanion}
        title={t('tools.companion')}
        sub={t('tools.companion_sub', { max: s.companion.max_per_hour })}
        onchange={flipCompanion}
      />
    {/key}
    {#if askingCompanion}
      <div class="ask" role="alertdialog" aria-label={t('tools.companion_ask')}>
        <b>{t('tools.companion_ask')}</b>
        <span class="grow"></span>
        <button class="bt" onclick={() => answerCompanion(false)}>{t('tools.companion_no')}</button>
        <button class="bt pri" onclick={() => answerCompanion(true)}>{t('tools.companion_yes')}</button>
      </div>
    {/if}
    {#if companionNote}<p class="note">{companionNote}</p>{/if}
    {#if s.companion.enabled}
      <div class="line time">
        <span>{t('tools.companion_every')}</span>
        <input class="fld" type="number" min="20" max="300" step="10" bind:value={companionEvery} onchange={() => setCompanion()} />
      </div>
      {#if !s.screen_enabled}<p class="note">{t('tools.companion_screen_off')}</p>{/if}
    {/if}
    <p class="hint tight">{t('tools.companion_cost')}</p>
    <Switch checked={s.tracking.app_time} title={t('tools.app_time')} sub={t('tools.app_time_sub')} onchange={(v) => setTracking({ app_time: v })} />
    <!-- Turning counting off keeps what was counted: it can be wiped either way. -->
    <div class="line"><button class="bt" onclick={() => run('clear_app_time')}>{t('tools.app_time_clear')}</button></div>
    <Switch checked={s.tracking.clipboard} title={t('tools.clipboard')} sub={t('tools.clipboard_sub')} onchange={(v) => setTracking({ clipboard: v })} />
    {#if s.tracking.clipboard}
      <div class="line time">
        <span>{t('tools.clipboard_count', { n: s.clip_count })}</span>
        <button class="bt" onclick={() => run('clear_clipboard_history')}>{t('tools.clipboard_clear')}</button>
      </div>
    {/if}
  </div>

  <div class="lbl">{t('tools.calendar')}</div>
  <div class="stack">
    <Switch checked={s.calendar_enabled} title={t('tools.calendar_on')} sub={t('tools.calendar_sub')} onchange={saveCalendar} />
  </div>
  <div class="line field">
    <input
      class="fld"
      type="password"
      autocomplete="off"
      spellcheck="false"
      placeholder={s.has_calendar_url ? t('tools.calendar_url_set') : t('tools.calendar_url')}
      bind:value={calendarUrl}
    />
    <button class="bt" onclick={() => s && saveCalendar(s.calendar_enabled)}>{t('tools.save')}</button>
  </div>
  <div class="line time">
    <span>{t('tools.calendar_remind')}</span>
    <input class="fld" type="number" min="0" max="120" bind:value={remind} onchange={() => s && saveCalendar(s.calendar_enabled)} />
  </div>
  {#if s.calendar_enabled && s.has_calendar_url}
    {#if s.calendar_error}
      <p class="err">{t('tools.calendar_error', { error: s.calendar_error })}</p>
    {:else if s.calendar_read_at}
      <p class="hint">{t('tools.calendar_status', { n: s.calendar_events, time: s.calendar_read_at })}</p>
    {:else}
      <p class="hint">{t('tools.calendar_wait')}</p>
    {/if}
  {/if}

  <div class="lbl">{t('tools.docs')}</div>
  <p class="hint">{t('tools.docs_sub')}</p>
  <div class="folders">
    {#each folders as f (f)}
      <div class="folder">
        <span class="path">{f}</span>
        <button class="bt small" disabled={savingFolders} onclick={() => s && setFolders(withoutFolder(s.docs_folders, f))}>{t('tools.docs_remove')}</button>
      </div>
    {/each}
  </div>
  <div class="line field">
    <input class="fld" spellcheck="false" placeholder={t('tools.docs_placeholder')} bind:value={newFolder} onkeydown={(e) => e.key === 'Enter' && addFolder()} />
    <button class="bt" disabled={savingFolders} onclick={addFolder}>{t('tools.docs_add')}</button>
  </div>
  {#if folders.length}
    <div class="line time">
      {#if s.docs.running}
        <span>
          {s.docs.progress
            ? t('tools.docs_running', { done: s.docs.progress[0], total: s.docs.progress[1] })
            : t('tools.docs_running_start')}
        </span>
      {:else}
        <span>{t('tools.docs_status', { files: s.docs.files, chunks: s.docs.chunks, embedded: s.docs.embedded })}</span>
        <button class="bt" onclick={() => run('reindex_docs')}>{t('tools.docs_reindex')}</button>
      {/if}
    </div>
    {#if s.docs.error}<p class="err">{s.docs.error}</p>{/if}
  {/if}
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .stack {
    display: grid;
    gap: 10px;
  }
  .card {
    padding: 12px 14px;
    border-radius: 12px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    display: grid;
    gap: 10px;
  }
  .ttl {
    font-size: 13.5px;
    font-weight: 600;
  }
  .sub {
    margin-top: 3px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .field {
    margin-top: 10px;
  }
  .field .fld {
    flex: 1;
    min-width: 0;
  }
  .time {
    margin: 4px 2px;
    font-size: 13px;
  }
  .time .fld {
    width: 90px;
  }
  .state {
    font-size: 13px;
    color: var(--ok);
  }
  .folders {
    display: grid;
    gap: 4px;
  }
  .folder {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.03);
    font-size: 13px;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    padding: 4px 10px;
    font-size: 12px;
  }
  .hint {
    margin: 4px 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .tight {
    margin: 0 2px;
  }
  .note {
    margin: 0 2px;
    font-size: 12.5px;
    color: var(--warn);
  }
  .ask {
    display: flex;
    flex-wrap: wrap;
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
  .err {
    margin: 6px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
