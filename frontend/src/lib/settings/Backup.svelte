<script lang="ts">
  // Transfer to another PC: save a copy, restore from one. The block only
  // draws what Rust returned and sends commands. A password lives in its field
  // until the command is sent; a key value never reaches this page at all.
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, tick } from 'svelte';
  import {
    addressWarns,
    initialSelection,
    keyLabel,
    offLabel,
    repeatLabel,
    scenarioComes,
    toggled,
    tokenWarns,
    type BackupFile,
    type BackupPreview,
    type CategoryId,
    type ExportResult,
    type KeyRow,
    type OffItem,
    type RestoreResult,
    type ScenarioRow,
    type TaskRow,
  } from '../backup';
  import { megabytes } from '../chat';
  import { actionLabel, pauseLabel, whenLabel } from '../events';
  import { t } from '../i18n';
  import { dateLocale } from '../locale.svelte';
  import { ruleLabel } from '../rules';

  let { name, mode = 'full' }: { name: string; mode?: 'full' | 'import' } = $props();

  type Step = 'idle' | 'export' | 'exported' | 'file' | 'preview' | 'done';

  // Shown in the hint; Rust checks the password itself.
  const MIN_PASSWORD = 10;

  let step = $state<Step>('idle');
  let busy = $state(false);
  let error = $state('');

  let protect = $state(true);
  let password = $state('');
  let repeat = $state('');
  let exported = $state<ExportResult | null>(null);

  let file = $state<BackupFile | null>(null);
  let unlock = $state('');
  let unlockField = $state<HTMLInputElement | null>(null);
  let preview = $state<BackupPreview | null>(null);
  let selected = $state<CategoryId[]>([]);
  let sure = $state(false);
  let result = $state<RestoreResult | null>(null);

  /** Changes the step; no password outlives the step it was typed in. */
  function go(to: Step) {
    step = to;
    password = '';
    repeat = '';
    unlock = '';
    error = '';
    sure = false;
  }

  const size = (bytes: number) => t('chat.av_mb', { value: megabytes(bytes) });
  const keyText = (k: KeyRow) => {
    const label = keyLabel(k);
    return t(label.key, label.params);
  };
  const offText = (o: OffItem) => {
    const label = offLabel(o);
    return t(label.key, label.params);
  };
  const has = (id: CategoryId) => selected.includes(id);

  /** The date of a copy as the user reads dates; a header that holds no date is shown as it is. */
  function when(created: string): string {
    const date = new Date(created);
    return Number.isNaN(date.getTime()) ? created : date.toLocaleString(dateLocale());
  }

  /** When a scheduled task runs next and, for one that repeats, how often. */
  function taskWhen(task: TaskRow): string {
    const at = new Date(task.due_at * 1000).toLocaleString(dateLocale(), { dateStyle: 'short', timeStyle: 'short' });
    const repeat = repeatLabel(task.repeat);
    return repeat ? `${at}, ${t(repeat.key, repeat.params)}` : at;
  }

  /** A scenario by event of the copy in one line: its event, what it does, its limits. */
  function scenarioText(s: ScenarioRow): string {
    return t('backup.scenario', {
      name: s.name,
      when: whenLabel(s),
      action: actionLabel(s.action),
      pause: pauseLabel(s.cooldown_s),
      max: s.max_per_day,
    });
  }

  // A scenario of a saved command comes back only with its command.
  const commandsInFile = $derived(preview?.categories.find((c) => c.id === 'commands')?.in_file ?? 0);

  /** Leaves a restore that was begun: Rust forgets the file and its key. */
  async function forget() {
    if (step === 'file' || step === 'preview') await invoke('backup_import_cancel').catch(() => {});
    file = null;
    preview = null;
  }

  // The page is closed in the middle of a restore: the key is not kept waiting.
  onDestroy(() => {
    if (step === 'file' || step === 'preview') invoke('backup_import_cancel').catch(() => {});
  });

  async function startExport() {
    await forget();
    go('export');
  }

  async function save() {
    busy = true;
    error = '';
    try {
      const made = await invoke<ExportResult | null>('backup_export', { protect, password, repeat });
      // Null: the save dialog was closed, the form stays.
      if (made) {
        exported = made;
        go('exported');
      }
    } catch (e) {
      error = String(e);
    } finally {
      password = '';
      repeat = '';
      busy = false;
    }
  }

  async function pick() {
    busy = true;
    error = '';
    // A new pick makes Rust forget the file picked before: what is on the
    // page about that file goes too, whatever the dialog brings.
    const leave = () => {
      file = null;
      preview = null;
      if (step === 'file' || step === 'preview') go('idle');
    };
    let picked: BackupFile | null = null;
    try {
      picked = await invoke<BackupFile | null>('backup_import_pick');
    } catch (e) {
      leave();
      error = String(e);
    }
    if (picked) {
      file = picked;
      preview = null;
      go('file');
      if (!picked.encrypted) await open(null);
    } else if (!error) {
      leave();
    }
    busy = false;
  }

  async function open(typed: string | null) {
    busy = true;
    error = '';
    try {
      const seen = await invoke<BackupPreview>('backup_import_preview', { password: typed });
      preview = seen;
      selected = initialSelection(seen);
      go('preview');
    } catch (e) {
      error = String(e);
    } finally {
      unlock = '';
      busy = false;
    }
    if (step === 'file') {
      // The field was disabled while the password was checked.
      await tick();
      unlockField?.focus();
    }
  }

  function toggle(id: CategoryId) {
    selected = toggled(selected, id);
    sure = false;
  }

  async function restore() {
    if (!sure) {
      sure = true;
      return;
    }
    busy = true;
    error = '';
    try {
      result = await invoke<RestoreResult>('backup_import_apply', { categories: selected });
      file = null;
      preview = null;
      go('done');
    } catch (e) {
      error = String(e);
      sure = false;
    } finally {
      busy = false;
    }
  }

  async function cancel() {
    await forget();
    go('idle');
  }
</script>

<div class="bk">
  {#if mode === 'full'}<b class="ttl">{t('backup.title')}</b>{/if}
  <p>{t('backup.sub')}</p>
  <div class="acts">
    {#if mode === 'full'}
      <button class="bt" onclick={startExport} disabled={busy}>{t('backup.export')}</button>
    {/if}
    <button class="bt" onclick={pick} disabled={busy}>{t('backup.import')}</button>
  </div>

  {#if step === 'export'}
    <div class="step">
      <label class="chk">
        <input type="checkbox" bind:checked={protect} disabled={busy} />{t('backup.protect')}
      </label>
      <p class="sub">{t('backup.protect_sub')}</p>
      {#if protect}
        <input
          class="fld"
          type="password"
          bind:value={password}
          placeholder={t('backup.password')}
          autocomplete="new-password"
          spellcheck="false"
          disabled={busy}
        />
        <input
          class="fld"
          type="password"
          bind:value={repeat}
          placeholder={t('backup.password_repeat')}
          autocomplete="new-password"
          spellcheck="false"
          disabled={busy}
        />
        <p class="sub">{t('backup.password_hint', { min: MIN_PASSWORD })}</p>
      {:else}
        <p class="warn">{t('backup.no_keys')}</p>
      {/if}
      {#if error}<p class="err">{error}</p>{/if}
      <div class="acts">
        <button class="bt pri" onclick={save} disabled={busy}>{t(busy ? 'backup.saving' : 'backup.save')}</button>
        <button class="bt" onclick={() => go('idle')} disabled={busy}>{t('backup.cancel')}</button>
      </div>
    </div>
  {:else if step === 'exported' && exported}
    <div class="step">
      <p class="say">{t('backup.saved', { path: exported.path, size: size(exported.bytes) })}</p>
      <p class="sub">
        {exported.encrypted ? t('backup.saved_keys', { count: exported.keys }) : t('backup.saved_plain')}
      </p>
      {#if error}<p class="err">{error}</p>{/if}
      <div class="acts">
        <button class="bt" onclick={() => invoke('backup_reveal').catch((e) => (error = String(e)))}>{t('backup.reveal')}</button>
      </div>
    </div>
  {:else if step === 'file' && file}
    <div class="step">
      <p class="say">
        {t('backup.file', {
          file: file.name,
          date: when(file.created_at),
          device: file.device,
          version: file.app_version,
          size: size(file.bytes),
        })}
      </p>
      <p class="sub">{file.encrypted ? t('backup.file_encrypted', { count: file.keys }) : t('backup.file_plain')}</p>
      {#if file.newer_app}<p class="warn">{t('backup.file_newer')}</p>{/if}
      {#if file.encrypted}
        <form
          onsubmit={(e) => {
            e.preventDefault();
            open(unlock);
          }}
        >
          <input
            class="fld"
            type="password"
            bind:this={unlockField}
            bind:value={unlock}
            placeholder={t('backup.password')}
            autocomplete="off"
            spellcheck="false"
            disabled={busy}
          />
          {#if error}<p class="err">{error}</p>{/if}
          <div class="acts">
            <button class="bt pri" type="submit" disabled={busy}>{t(busy ? 'backup.unlocking' : 'backup.unlock')}</button>
            <button class="bt" type="button" onclick={cancel} disabled={busy}>{t('backup.cancel')}</button>
          </div>
        </form>
      {:else}
        {#if busy}<p class="sub">{t('backup.reading')}</p>{/if}
        {#if error}<p class="err">{error}</p>{/if}
        <div class="acts">
          <button class="bt" onclick={cancel} disabled={busy}>{t('backup.cancel')}</button>
        </div>
      {/if}
    </div>
  {:else if step === 'preview' && preview}
    <div class="step">
      <b>{t('backup.preview_title')}</b>
      <p class="sub">{t('backup.preview_sub')}</p>
      <div class="cats">
        {#each preview.categories as c (c.id)}
          <label class="cat" class:none={!c.available}>
            <input type="checkbox" checked={has(c.id)} disabled={!c.available || busy} onchange={() => toggle(c.id)} />
            <span class="nm">{t(`backup.cat.${c.id}`)}</span>
            <span class="ct">{c.available ? t('backup.count', { file: c.in_file, here: c.here }) : t('backup.cat_none')}</span>
          </label>
          {#if c.id === 'tasks' && c.available}
            <div class="under">
              <p class="sub">{t('backup.tasks_sub', { name })}</p>
              {#each preview.tasks as task}
                <div class="mono">{task.text} · {taskWhen(task)}</div>
              {/each}
              {#if preview.tasks_total > preview.tasks.length}
                <p class="sub">{t('backup.tasks_more', { count: preview.tasks_total - preview.tasks.length })}</p>
              {/if}
            </div>
          {/if}
          {#if c.id === 'scenarios' && c.available}
            <div class="under">
              <p class="sub">{t('backup.scenarios_sub', { name })}</p>
              {#each preview.scenarios as s}
                <div class="mono">{scenarioText(s)}</div>
                {#if has('scenarios') && !scenarioComes(s, selected, commandsInFile)}
                  <p class="warn">{t('backup.scenario_no_command', { command: s.action.text })}</p>
                {/if}
              {/each}
              {#if preview.scenarios_total > preview.scenarios.length}
                <p class="sub">{t('backup.scenarios_more', { count: preview.scenarios_total - preview.scenarios.length })}</p>
              {/if}
            </div>
          {/if}
          {#if c.id === 'rules_deny' && c.available}
            <div class="under"><p class="sub">{t('backup.rules_deny_sub')}</p></div>
          {/if}
          {#if c.id === 'rules_allow' && (c.available || preview.allow_rules_dropped > 0)}
            <div class="under">
              {#if c.available}<p class="sub">{t('backup.rules_allow_sub', { name })}</p>{/if}
              {#each preview.allow_rules as r}
                {#if ruleLabel(r.pattern)}
                  <div title={r.pattern}>{r.tool} · {t(ruleLabel(r.pattern)!)}</div>
                {:else}
                  <div class="mono">{r.tool} · {r.pattern}</div>
                {/if}
              {/each}
              {#if preview.allow_rules_total > preview.allow_rules.length}
                <p class="sub">{t('backup.rules_more', { count: preview.allow_rules_total - preview.allow_rules.length })}</p>
              {/if}
              {#if preview.allow_rules_dropped > 0}
                <p class="sub">{t('backup.rules_dropped', { count: preview.allow_rules_dropped })}</p>
              {/if}
            </div>
          {/if}
          {#if c.id === 'secrets' && preview.secrets.length > 0}
            <div class="under">
              {#each preview.secrets as k}
                <div class="line">
                  {keyText(k)}{#if k.replaces}<span class="tag">{t('backup.key_replaces')}</span>{/if}
                </div>
              {/each}
            </div>
          {/if}
        {/each}
      </div>

      {#if has('settings') && (preview.addresses.length > 0 || preview.mcp.length > 0 || preview.telegram)}
        <div class="blk">
          <b>{t('backup.check_title')}</b>
          {#each preview.addresses as a}
            <div class="line">{t(`backup.address.${a.purpose}`, { url: a.incoming })}</div>
            {#if addressWarns(a, selected)}<p class="warn">{t('backup.address_warn', { current: a.current })}</p>{/if}
          {/each}
          {#each preview.mcp as m}
            <div class="line">{t('backup.mcp', { name: m.name, target: m.target })}</div>
            {#if tokenWarns(m, selected)}<p class="warn">{t('backup.mcp_warn')}</p>{/if}
          {/each}
          {#if preview.telegram}
            <div class="line">{t('backup.telegram', { id: preview.telegram.user_id })}</div>
            {#if tokenWarns(preview.telegram, selected)}<p class="warn">{t('backup.telegram_warn')}</p>{/if}
          {/if}
        </div>
      {/if}

      {#if has('settings')}
        {#if preview.switched_off.length > 0}
          <div class="blk">
            <b>{t('backup.off_title')}</b>
            <p class="sub">{t('backup.off_sub')}</p>
            {#each preview.switched_off as o}
              <div class="line">{offText(o)}</div>
            {/each}
          </div>
        {/if}
        {#if preview.docs_folders.length > 0}
          <p class="sub">{t('backup.docs_folders', { folders: preview.docs_folders.join(', ') })}</p>
        {/if}
        <p class="sub">{t('backup.kept')}</p>
      {/if}

      {#if preview.schema_newer}<p class="warn">{t('backup.file_newer')}</p>{/if}
      {#if preview.skipped_unknown > 0}<p class="sub">{t('backup.skipped', { count: preview.skipped_unknown })}</p>{/if}
      {#if preview.dropped_rows > 0}<p class="sub">{t('backup.dropped', { count: preview.dropped_rows })}</p>{/if}
      <p class="sub">{t('backup.safety', { path: preview.safety_copy })}</p>
      {#if error}<p class="err">{error}</p>{/if}
      <div class="acts">
        <button class="bt no" onclick={restore} disabled={busy || selected.length === 0}>
          {t(busy ? 'backup.restoring' : sure ? 'backup.restore_sure' : 'backup.restore')}
        </button>
        <button class="bt" onclick={cancel} disabled={busy}>{t('backup.cancel')}</button>
      </div>
    </div>
  {:else if step === 'done' && result}
    <div class="step">
      <b>{t('backup.done')}</b>
      {#each result.restored as r (r.id)}
        <div class="line">{t(`backup.cat.${r.id}`)}: {r.count}</div>
      {/each}
      {#if result.keys_written > 0}<p class="sub">{t('backup.done_keys', { count: result.keys_written })}</p>{/if}
      {#if result.keys_failed.length > 0}
        <p class="err">{t('backup.done_keys_failed', { list: result.keys_failed.map(keyText).join(', ') })}</p>
      {/if}
      {#if result.config_error}<p class="err">{t('backup.done_config_failed', { why: result.config_error })}</p>{/if}
      {#if result.scenarios_without_command > 0}
        <p class="sub">{t('backup.done_scenarios_skipped', { count: result.scenarios_without_command })}</p>
      {/if}
      {#if result.switched_off.length > 0}
        <div class="blk">
          <b>{t('backup.off_title')}</b>
          <p class="sub">{t('backup.off_sub')}</p>
          {#each result.switched_off as o}
            <div class="line">{offText(o)}</div>
          {/each}
        </div>
      {/if}
      {#if result.docs_folders.length > 0}
        <p class="sub">{t('backup.docs_folders', { folders: result.docs_folders.join(', ') })}</p>
      {/if}
      {#if error}<p class="err">{error}</p>{/if}
      {#if result.restart_needed}
        <p class="say">{t('backup.restart_needed', { name })}</p>
        <div class="acts">
          <button class="bt pri" onclick={() => invoke('backup_restart').catch((e) => (error = String(e)))}>{t('backup.restart')}</button>
        </div>
      {/if}
    </div>
  {:else if error}
    <p class="err">{error}</p>
  {/if}
</div>

<style>
  /* The look of the «report» block on the general page. */
  .bk {
    padding: 14px 16px;
    margin-bottom: 12px;
    border-radius: 16px;
    border: 1px solid var(--line);
  }
  .ttl {
    display: block;
    margin-bottom: 6px;
    font-size: 14px;
  }
  p {
    margin: 0 0 12px;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .step {
    margin-top: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
  }
  .step > b,
  .blk > b {
    display: block;
    margin-bottom: 6px;
    font-size: 13px;
  }
  .say {
    color: var(--ink);
    user-select: text;
    word-break: break-word;
  }
  .sub {
    user-select: text;
    word-break: break-word;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .chk input,
  .cat input {
    accent-color: var(--v);
  }
  .fld {
    margin-bottom: 8px;
  }
  .warn {
    color: var(--warn);
  }
  .err {
    color: var(--bad);
    user-select: text;
    word-break: break-word;
  }
  .cats {
    margin-bottom: 12px;
  }
  .cat {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 2px;
    border-top: 1px solid var(--line);
    font-size: 13px;
    cursor: pointer;
  }
  .cat:first-child {
    border-top: 0;
  }
  .cat.none {
    color: var(--dim);
    cursor: default;
  }
  .nm {
    flex: 1;
  }
  .ct {
    font-size: 12px;
    color: var(--mute);
    white-space: nowrap;
  }
  .under {
    margin: 0 0 8px 24px;
  }
  .under p {
    margin-bottom: 6px;
  }
  .under .warn {
    margin: 0 0 6px;
  }
  .blk {
    margin-bottom: 12px;
    padding: 10px 12px;
    border-radius: 10px;
    background: rgba(0, 0, 0, 0.25);
  }
  .blk p {
    margin-bottom: 6px;
  }
  .blk .warn {
    margin: 2px 0 8px;
  }
  .line {
    font-size: 12.5px;
    line-height: 1.6;
    user-select: text;
    word-break: break-word;
  }
  .mono {
    font: 11.5px/1.6 var(--mono);
    user-select: text;
    word-break: break-word;
  }
  .tag {
    margin-left: 8px;
    font-size: 11.5px;
    color: var(--warn);
  }
</style>
