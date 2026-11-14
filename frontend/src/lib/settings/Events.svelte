<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';
  import { actionLabel, minutesLeft, pauseLabel, waitLabel, whenLabel, type EventsView, type TriggerItem } from '../events';

  let view = $state<EventsView | null>(null);
  // The moment of the last reading: the minutes left of a watch are counted from it.
  let now = $state(Date.now());
  // Two kinds of trouble, cleared apart: a refresh that failed is forgotten by the next good one,
  // a refused action stays until the next action.
  let loadError = $state('');
  let actionError = $state('');
  const error = $derived(actionError || loadError);
  let timer: ReturnType<typeof setInterval> | undefined;
  // An answer that comes back after a newer request was sent is dropped.
  let asked = 0;

  onMount(() => {
    load();
    // Scenarios run and watches end while the page is open.
    timer = setInterval(load, 10_000);
  });
  onDestroy(() => clearInterval(timer));

  async function load() {
    const mine = ++asked;
    try {
      const fresh = await invoke<EventsView>('get_events');
      if (mine !== asked) return;
      view = fresh;
      now = Date.now();
      loadError = '';
    } catch (e) {
      if (mine === asked) loadError = String(e);
    }
  }

  /** True when the command went through. */
  async function run(command: string, args: Record<string, unknown>): Promise<boolean> {
    let ok = true;
    try {
      await invoke(command, args);
      actionError = '';
    } catch (e) {
      actionError = String(e);
      ok = false;
    }
    await load();
    return ok;
  }

  async function toggle(tr: TriggerItem, box: HTMLInputElement) {
    // A refused change puts the mark back: the list did not change, so nothing redraws it.
    if (!(await run('set_trigger_enabled', { id: tr.id, enabled: box.checked }))) box.checked = tr.enabled;
  }

  function limits(tr: TriggerItem): string {
    return t('events.limits', { pause: pauseLabel(tr.cooldown_s), max: tr.max_per_day, count: tr.fired_today });
  }
</script>

{#if view}
  <div class="lbl">{t('events.title')}</div>
  <p class="hint">{t('events.sub')}</p>
  <Switch checked={view.enabled} title={t('events.enable')} sub={t('events.enable_sub', { n: view.max_per_hour })} onchange={(v) => run('set_events_enabled', { value: v })} />
  <div class="list">
    {#if view.triggers.length === 0}
      <p class="hint">{t('events.none')}</p>
    {/if}
    {#each view.triggers as tr (tr.id)}
      <div class="item">
        <input type="checkbox" checked={tr.enabled} aria-label={t('events.switch')} title={t('events.switch')} onchange={(e) => toggle(tr, e.currentTarget)} />
        <div class="txt">
          <b>
            {tr.name}
            <i class="tag">{whenLabel(tr)}</i>
            {#if !tr.enabled}<i class="tag dim">{t('events.off')}</i>{/if}
            {#if tr.command_missing}<i class="tag bad">{t('events.command_missing')}</i>{/if}
            {#if tr.command_changed}<i class="tag bad">{t('events.command_changed')}</i>{/if}
          </b>
          <small>{actionLabel(tr.action)} · {limits(tr)}</small>
          {#if tr.command_text}<pre class="cmd" title={t('events.command_text')} aria-label={t('events.command_text')}>{tr.command_text}</pre>{/if}
        </div>
        <button class="ib" title={t('events.remove')} aria-label={t('events.remove')} onclick={() => run('delete_trigger', { id: tr.id })}><Icon name="x" size={14} /></button>
      </div>
    {/each}
  </div>

  <div class="lbl">{t('events.waits')}</div>
  <p class="hint">{t('events.waits_sub')}</p>
  {#if view.waits.length === 0}
    <p class="hint">{t('events.waits_none')}</p>
  {/if}
  {#each view.waits as w (w.id)}
    <div class="item">
      <div class="txt">
        <b>{waitLabel(w)}<i class="tag dim">{t('events.wait_left', { n: minutesLeft(w.until, now) })}</i></b>
      </div>
      <button class="ib" title={t('events.wait_stop')} aria-label={t('events.wait_stop')} onclick={() => run('stop_wait', { id: w.id })}><Icon name="x" size={14} /></button>
    </div>
  {/each}
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  /* The rules of the rows are those of Extensions.svelte: styles do not cross components. */
  .hint {
    margin: 0 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .list {
    margin-top: 10px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 8px 9px 14px;
    margin-bottom: 6px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
  }
  .item input[type='checkbox'] {
    flex: none;
    accent-color: var(--v);
  }
  .txt {
    flex: 1;
    min-width: 0;
  }
  .txt b {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: 13.5px;
    overflow-wrap: anywhere;
    user-select: text;
  }
  /* The whole instruction stays readable: a scenario runs when nobody is looking. */
  .txt small {
    display: block;
    margin-top: 2px;
    color: var(--mute);
    font-size: 12.5px;
    overflow-wrap: anywhere;
    user-select: text;
  }
  /* What the saved command says: a scenario switched on here runs this text by itself.
     A long one scrolls inside its box, so the list keeps its shape. */
  .txt .cmd {
    margin: 6px 0 1px;
    max-height: 96px;
    padding: 6px 9px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: rgba(0, 0, 0, 0.28);
    color: var(--mute);
    font: 12px/1.45 var(--mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    overflow: auto;
    user-select: text;
  }
  .item .ib {
    width: 26px;
    height: 26px;
  }
  .tag {
    font-style: normal;
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 999px;
    color: var(--c);
    background: rgba(111, 227, 255, 0.1);
  }
  .tag.dim {
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
  }
  .tag.bad {
    color: var(--bad);
    background: rgba(255, 107, 125, 0.12);
  }
  .lbl {
    margin-top: 22px;
  }
  .err {
    margin: 12px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
