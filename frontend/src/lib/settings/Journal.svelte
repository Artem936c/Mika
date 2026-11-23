<script lang="ts">
  // The journal of the app: its rows with a filter, live rows while «follow» is on, the files
  // and what is written. The page draws what Rust returned and sends commands; what can be
  // counted without the page lives in journal.ts.
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';
  import {
    CATEGORIES,
    LEVELS,
    MAX_KEEP_DAYS,
    MAX_ROWS,
    MIN_KEEP_DAYS,
    SEARCH_MS,
    SURE_MS,
    TAIL_MS,
    appendPage,
    buildQuery,
    canShowMore,
    categoryLabel,
    cleanDays,
    dayKey,
    emptyFilter,
    exportQuery,
    filterActive,
    folded,
    isLong,
    mergeTail,
    needsReload,
    pageOf,
    rowKeys,
    rowLine,
    rowTime,
    settingsOf,
    sizeLine,
    tailCursor,
    tailOf,
    toggleLevel,
    withCategory,
    withText,
    type JournalFilter,
    type JournalRow,
    type JournalSettings,
    type JournalView,
    type Level,
  } from '../journal';

  let view = $state<JournalView | null>(null);
  /** Newest first. Never longer than `MAX_ROWS`. */
  let rows = $state.raw<JournalRow[]>([]);
  /** The mark of the page under the list. */
  let next = $state<string | null>(null);
  /** What the controls show. */
  let filter = $state.raw<JournalFilter>(emptyFilter());
  /** What the list was read with: live rows are matched against it. */
  let applied = $state.raw<JournalFilter>(emptyFilter());
  /** The filter of the read that is on its way, if any. */
  let pending: JournalFilter | null = null;
  let follow = $state(true);
  let loading = $state(true);
  let loadingMore = $state(false);
  /** The list was read at least once. */
  let loaded = $state(false);
  let today = $state(dayKey(new Date()));
  /** Keys of the long rows shown whole. */
  let unfolded = $state.raw(new Set<string>());
  /** Key of the row that was just copied. */
  let copied = $state('');
  /** The previous run was cut short: said once, on the page that was opened first. */
  let crashNote = $state(false);
  /** The second press on the clear button clears. */
  let sure = $state(false);
  let busy = $state(false);
  let note = $state('');
  /** The warning of the content switch is on screen; nothing is sent until it is answered. */
  let contentAsk = $state(false);
  /** A refused change draws the settings anew, from what is saved. */
  let rev = $state(0);
  let listError = $state('');
  let tailError = $state('');
  let actionError = $state('');
  let settingsError = $state('');

  // An answer that comes back after a newer read was sent is dropped.
  let asked = 0;
  /** The number of the last live row seen. */
  let after: number | null = null;
  let polling = false;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  let sureTimer: ReturnType<typeof setTimeout> | undefined;
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  const keys = $derived(rowKeys(rows));
  const capped = $derived(rows.length >= MAX_ROWS);

  onMount(() => {
    loadView();
    reload();
  });

  onDestroy(() => {
    asked += 1;
    clearTimeout(searchTimer);
    clearTimeout(sureTimer);
    clearTimeout(copyTimer);
  });

  // New rows are asked for only while the page is open and the switch is on.
  $effect(() => {
    if (!follow) return;
    const timer = setInterval(poll, TAIL_MS);
    return () => clearInterval(timer);
  });

  async function loadView() {
    try {
      const fresh = await invoke<JournalView | null>('journal_view');
      if (!fresh) return;
      view = fresh;
      settingsError = '';
      if (fresh.crashed) {
        crashNote = true;
        invoke('journal_crash_seen').catch((e) => (actionError = String(e)));
      }
    } catch (e) {
      settingsError = String(e);
    }
  }

  /** Reads the newest rows of the filter: the list starts over, the page mark too. */
  async function reload() {
    clearTimeout(searchTimer);
    const mine = ++asked;
    const wanted = filter;
    pending = wanted;
    loading = true;
    try {
      const page = pageOf(await invoke<unknown>('journal_read', { query: buildQuery(wanted) }));
      if (mine !== asked) return;
      rows = page.rows.slice(0, MAX_ROWS);
      next = page.next;
      applied = wanted;
      after = tailCursor(after, rows);
      unfolded = new Set();
      today = dayKey(new Date());
      listError = '';
      loaded = true;
    } catch (e) {
      if (mine === asked) listError = String(e);
    } finally {
      if (mine === asked) {
        loading = false;
        pending = null;
      }
    }
  }

  async function more() {
    if (next === null || loadingMore) return;
    const mine = asked;
    loadingMore = true;
    try {
      const page = pageOf(await invoke<unknown>('journal_read', { query: buildQuery(applied, next) }));
      if (mine !== asked) return;
      rows = appendPage(rows, page.rows);
      next = page.next;
      listError = '';
    } catch (e) {
      if (mine === asked) listError = String(e);
    } finally {
      loadingMore = false;
    }
  }

  async function poll() {
    if (polling || loading || !loaded) return;
    polling = true;
    const mine = asked;
    try {
      const tail = tailOf(await invoke<unknown>('journal_tail', { after }));
      if (mine !== asked) return;
      after = tailCursor(after, tail.rows, tail.last);
      today = dayKey(new Date());
      rows = mergeTail(rows, tail.rows, applied);
      tailError = '';
    } catch (e) {
      if (mine === asked) tailError = String(e);
    } finally {
      polling = false;
    }
  }

  /** A filter set by a click is read at once. */
  function pick(to: JournalFilter) {
    filter = to;
    reload();
  }

  /** The search waits until the typing stops. */
  function typed(text: string) {
    filter = withText(filter, text);
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      if (needsReload(filter, applied, pending)) reload();
    }, SEARCH_MS);
  }

  function toggleFollow() {
    follow = !follow;
    tailError = '';
    // What was written while nobody followed is read from the files.
    if (follow) reload();
  }

  function toggleFold(key: string) {
    const to = new Set(unfolded);
    if (!to.delete(key)) to.add(key);
    unfolded = to;
  }

  async function copy(row: JournalRow, key: string) {
    try {
      await navigator.clipboard.writeText(rowLine(row));
      copied = key;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = ''), 1500);
    } catch (e) {
      listError = String(e);
    }
  }

  async function exportFile() {
    busy = true;
    note = '';
    try {
      // Null: the save dialog was closed.
      const path = await invoke<string | null>('journal_export', { query: exportQuery(applied) });
      if (path) note = t('journal.exported', { path });
      actionError = '';
    } catch (e) {
      actionError = String(e);
    } finally {
      busy = false;
    }
  }

  async function openFolder() {
    try {
      await invoke('open_folder', { which: 'logs' });
      actionError = '';
    } catch (e) {
      actionError = String(e);
    }
  }

  async function clear() {
    if (!sure) {
      sure = true;
      clearTimeout(sureTimer);
      sureTimer = setTimeout(() => (sure = false), SURE_MS);
      return;
    }
    clearTimeout(sureTimer);
    sure = false;
    busy = true;
    note = '';
    try {
      view = await invoke<JournalView>('journal_clear');
      actionError = '';
      rows = [];
      next = null;
      await reload();
    } catch (e) {
      // Cleared in part: the memory is empty and some files are gone. The list and the size
      // line are read again, so the error stands over what is really left.
      rows = [];
      next = null;
      await reload();
      await loadView();
      actionError = String(e);
    } finally {
      busy = false;
    }
  }

  /** True when the settings were saved. */
  async function save(change: Partial<JournalSettings>): Promise<boolean> {
    if (!view) return false;
    try {
      view = await invoke<JournalView>('set_journal', { settings: { ...settingsOf(view), ...change } });
      settingsError = '';
      return true;
    } catch (e) {
      settingsError = String(e);
      rev += 1;
      return false;
    }
  }

  function saveDays(box: HTMLInputElement) {
    if (!view) return;
    const days = cleanDays(box.value, view.keep_days);
    box.value = String(days);
    if (days !== view.keep_days) save({ keep_days: days });
  }

  /** Turning the content on asks first; turning it off is sent at once. */
  function setContent(on: boolean) {
    if (!view) return;
    if (on) {
      contentAsk = true;
      return;
    }
    contentAsk = false;
    if (view.content) save({ content: false });
  }

  async function confirmContent() {
    await save({ content: true });
    contentAsk = false;
  }
</script>

{#if crashNote}<p class="calm">{t('journal.crash_here')}</p>{/if}

<div class="bar">
  <div class="chips" role="group" aria-label={t('journal.levels')}>
    {#each LEVELS as level (level)}
      {@const on = filter.levels.includes(level)}
      <button class="tg" class:on aria-pressed={on} onclick={() => pick(toggleLevel(filter, level))}>
        <i class="dot {level}"></i>{t(`journal.level.${level}`)}
      </button>
    {/each}
  </div>
  <div class="find">
    <select
      class="fld"
      aria-label={t('journal.category')}
      title={t('journal.category')}
      value={filter.categories[0] ?? ''}
      onchange={(e) => pick(withCategory(filter, e.currentTarget.value))}
    >
      <option value="">{t('journal.cat_all')}</option>
      {#each CATEGORIES as category (category)}
        <option value={category}>{categoryLabel(category)}</option>
      {/each}
    </select>
    <input
      class="fld"
      type="search"
      spellcheck="false"
      autocomplete="off"
      placeholder={t('journal.search')}
      aria-label={t('journal.search_title')}
      title={t('journal.search_title')}
      value={filter.text}
      oninput={(e) => typed(e.currentTarget.value)}
      onkeydown={(e) => e.key === 'Enter' && reload()}
    />
    <button class="tg" class:on={follow} aria-pressed={follow} title={t('journal.follow_title')} onclick={toggleFollow}>
      <i class="live" class:on={follow}></i>{t('journal.follow')}
    </button>
  </div>
</div>

<div class="list" class:wait={loading && loaded} role="region" aria-label={t('journal.list')} aria-busy={loading}>
  {#if rows.length === 0}
    <div class="none">
      {#if !loaded}
        {#if loading}<span>{t('journal.loading')}</span>{/if}
      {:else if filterActive(applied)}
        <span>{t('journal.empty_filter')}</span>
        <button class="link" onclick={() => pick(emptyFilter())}>{t('journal.reset')}</button>
      {:else}
        <span>{t('journal.empty')}</span>
      {/if}
    </div>
  {:else}
    <div class="rows">
      {#each rows as row, i (keys[i])}
        {@const key = keys[i]}
        {@const long = isLong(row.message)}
        {@const whole = !long || unfolded.has(key)}
        <div class="row {row.level}">
          <span class="at" title={row.t}>{rowTime(row.t, today)}</span>
          <span class="lv">{t(`journal.tag.${row.level}`)}</span>
          <span class="cat" title={row.category}>{categoryLabel(row.category)}</span>
          <span class="msg"
            >{whole ? row.message : folded(row.message)}{#if long}{' '}<button class="link" aria-expanded={whole} onclick={() => toggleFold(key)}
                >{t(whole ? 'journal.fold' : 'journal.unfold')}</button
              >{/if}</span
          >
          <button
            class="cp"
            class:done={copied === key}
            title={t(copied === key ? 'journal.copied' : 'journal.copy')}
            aria-label={t(copied === key ? 'journal.copied' : 'journal.copy')}
            onclick={() => copy(row, key)}
          >
            <Icon name={copied === key ? 'check' : 'copy'} size={13} />
          </button>
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if rows.length > 0}
  <div class="foot">
    <span class="count">{t(capped ? 'journal.capped' : 'journal.shown', { count: rows.length })}</span>
    {#if canShowMore(rows.length, next)}
      <button class="bt small" disabled={loadingMore || loading} onclick={more}>{t('journal.more')}</button>
    {/if}
  </div>
{/if}
{#if listError || tailError}<p class="err">{listError || tailError}</p>{/if}

<div class="acts">
  <button class="bt" disabled={busy} onclick={exportFile}>{t('journal.export')}</button>
  <button class="bt" onclick={openFolder}>{t('journal.folder')}</button>
  <button class="bt no" disabled={busy} onclick={clear}>{t(sure ? 'journal.clear_sure' : 'journal.clear')}</button>
</div>
{#if note}<p class="ok">{note}</p>{/if}
{#if actionError}<p class="err">{actionError}</p>{/if}

{#if view}
  <p class="hint size" title={view.dir}>{sizeLine(view)}</p>

  <div class="lbl">{t('journal.settings')}</div>
  {#key rev}
    <div class="form">
      <label>
        <span>{t('journal.write')}</span>
        <select class="fld" value={view.level} onchange={(e) => save({ level: e.currentTarget.value as Level })}>
          {#each LEVELS as level (level)}
            <option value={level}>{t(`journal.write.${level}`)}</option>
          {/each}
        </select>
      </label>
      <label>
        <span>{t('journal.keep')}</span>
        <input
          class="fld days"
          type="number"
          min={MIN_KEEP_DAYS}
          max={MAX_KEEP_DAYS}
          step="1"
          value={view.keep_days}
          onchange={(e) => saveDays(e.currentTarget)}
          onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
        />
      </label>
    </div>
    <p class="hint">{t('journal.keep_sub', { files: view.keep_files })}</p>
    <Switch checked={view.content || contentAsk} title={t('journal.content')} sub={t('journal.content_sub')} onchange={setContent} />
  {/key}
  {#if contentAsk}
    <div class="ask">
      <p>{t('journal.content_warn')}</p>
      <div class="acts">
        <button class="bt pri" onclick={confirmContent}>{t('journal.content_yes')}</button>
        <button class="bt" onclick={() => (contentAsk = false)}>{t('journal.content_no')}</button>
      </div>
    </div>
  {/if}
{/if}
{#if settingsError}<p class="err">{settingsError}</p>{/if}

<style>
  .calm {
    margin: 0 0 12px;
    padding: 10px 14px;
    border-radius: 12px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.04);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
    user-select: text;
  }
  .bar {
    display: grid;
    gap: 8px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .find {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .find .fld {
    height: 34px;
    padding: 0 12px;
    font-size: 13px;
  }
  /* The three stay in one line down to the narrowest window. */
  .find select.fld {
    flex: 0 1 160px;
    width: auto;
    min-width: 0;
  }
  .find input.fld {
    flex: 1 1 70px;
    width: auto;
    min-width: 0;
  }
  .find .tg {
    height: 34px;
  }
  select.fld {
    appearance: none;
    cursor: pointer;
  }
  select.fld option {
    background: var(--raised);
  }
  .dot,
  .live {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--dim);
  }
  .dot.error {
    background: var(--bad);
  }
  .dot.warn {
    background: var(--warn);
  }
  .dot.info {
    background: var(--c);
  }
  .live.on {
    background: var(--ok);
    box-shadow: 0 0 8px var(--ok);
  }

  /* The list scrolls in its own box, as tall as the page has room for; the page under it
     keeps the buttons and the settings in reach. The box is the container the rows ask
     about its width. */
  .list {
    container-type: inline-size;
    margin-top: 10px;
    max-height: max(200px, calc(100cqh - 210px));
    overflow-y: auto;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(0, 0, 0, 0.28);
    transition: opacity 0.2s;
  }
  .list.wait {
    opacity: 0.6;
  }
  .none {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 4px 12px;
    min-height: 19px;
    padding: 28px 14px;
    font-size: 12.5px;
    color: var(--dim);
  }
  /* One grid for all rows: a column is as wide as its widest cell. */
  .rows {
    display: grid;
    grid-template-columns: auto auto auto minmax(0, 1fr) auto;
  }
  .row {
    grid-column: 1 / -1;
    display: grid;
    grid-template-columns: subgrid;
    align-items: baseline;
    column-gap: 10px;
    padding: 5px 6px 5px 12px;
    border-top: 1px solid var(--line);
    font-size: 12px;
  }
  .row:first-child {
    border-top: 0;
  }
  .row.error {
    box-shadow: inset 2px 0 0 var(--bad);
    background: rgba(255, 107, 125, 0.05);
  }
  .row.warn {
    box-shadow: inset 2px 0 0 var(--warn);
  }
  .at {
    font: 11.5px/1.5 var(--mono);
    color: var(--dim);
    white-space: nowrap;
    user-select: text;
  }
  .lv {
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    white-space: nowrap;
    color: var(--dim);
  }
  .row.error .lv {
    color: var(--bad);
  }
  .row.warn .lv {
    color: var(--warn);
  }
  .row.info .lv {
    color: var(--c);
  }
  .cat {
    font-size: 11.5px;
    color: var(--mute);
    white-space: nowrap;
  }
  .msg {
    min-width: 0;
    font: 11.5px/1.5 var(--mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }
  .row.debug .msg {
    color: var(--mute);
  }
  .cp {
    all: unset;
    box-sizing: border-box;
    align-self: start;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 7px;
    color: var(--mute);
    cursor: pointer;
    opacity: 0.35;
    transition:
      opacity 0.2s,
      color 0.2s;
  }
  .row:hover .cp,
  .cp:focus-visible,
  .cp.done {
    opacity: 1;
  }
  .cp:hover {
    color: #fff;
    background: rgba(255, 255, 255, 0.08);
  }
  .cp.done {
    color: var(--ok);
  }
  .cp:focus-visible {
    outline: 2px solid rgba(var(--v-rgb), 0.7);
    outline-offset: 1px;
  }
  /* A narrow window: the message takes its own line under the time and the marks. */
  @container (max-width: 500px) {
    .rows {
      display: block;
    }
    .row {
      grid-template-columns: auto auto minmax(0, 1fr) auto;
      grid-template-areas:
        'at lv cat cp'
        'msg msg msg msg';
      row-gap: 1px;
    }
    .at {
      grid-area: at;
    }
    .lv {
      grid-area: lv;
    }
    .cat {
      grid-area: cat;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .msg {
      grid-area: msg;
    }
    .cp {
      grid-area: cp;
    }
  }
  .link {
    all: unset;
    cursor: pointer;
    font-weight: 600;
    color: var(--v);
    white-space: nowrap;
    user-select: none;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link:focus-visible {
    outline: 2px solid rgba(var(--v-rgb), 0.7);
    outline-offset: 2px;
  }
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 6px 12px;
    min-height: 30px;
    margin: 8px 2px 0;
  }
  .count {
    flex: 1;
    min-width: 160px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .small {
    height: 30px;
    padding: 0 11px;
    font-size: 12px;
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 14px;
  }
  .hint {
    margin: 8px 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .size {
    margin-top: 10px;
    user-select: text;
  }
  .ok {
    margin: 8px 2px 0;
    font-size: 12.5px;
    color: var(--ok);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .lbl {
    margin-top: 22px;
  }
  .form {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px;
  }
  .form label {
    display: grid;
    gap: 4px;
    min-width: 0;
    font-size: 12px;
    color: var(--mute);
  }
  .form .fld {
    width: 240px;
    max-width: 100%;
    height: 36px;
  }
  .form .days {
    width: 110px;
  }
  .ask {
    margin-top: 8px;
    padding: 12px 16px 14px;
    border-radius: 16px;
    border: 1px solid rgba(255, 198, 92, 0.3);
    background: rgba(255, 198, 92, 0.06);
  }
  .ask p {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--warn);
  }
  .ask .acts {
    margin-top: 12px;
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    overflow-wrap: anywhere;
    user-select: text;
  }
</style>
