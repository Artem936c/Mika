<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { comboFromEvent, type HotkeysView } from '../hotkeys';
  import { isMessageKey, t } from '../i18n';

  /** A combination is sent this long after its key went down, if no key goes up sooner. */
  const RELEASE_WAIT_MS = 800;

  let v = $state<HotkeysView | null>(null);
  let error = $state('');
  /** Action whose combination is being chosen now. */
  let editing = $state('');
  /** The combination just pressed: it is sent when a key goes up. */
  let pressed = $state('');
  let sendTimer: ReturnType<typeof setTimeout> | undefined;
  /** Bumped when a choice is cancelled: an answer that comes after belongs to nobody. */
  let turn = 0;

  const comboOf = (id: string) => v?.hotkeys.find((h) => h.id === id)?.combo ?? '';
  /** A pressed combination without the browser prefix of its key: `KeyD` reads `D`. */
  const shown = (combo: string) => combo.replace(/(?:Key|Digit)(\w)$/, '$1');

  function actionName(id: string): string {
    const key = `hotkeys.action.${id}`;
    return isMessageKey(key) ? t(key) : id;
  }

  /** What the hotkey of an action waits for: the microphone or the screen switch. */
  function needsName(id: string): string {
    const key = `hotkeys.status_needs.${id}`;
    return isMessageKey(key) ? t(key) : t('hotkeys.status_off');
  }

  async function run(cmd: string, args: Record<string, unknown>) {
    try {
      v = await invoke<HotkeysView>(cmd, args);
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  /** The global hotkeys step aside while a new one is pressed, so that the keys reach the page. */
  function pause(paused: boolean) {
    return invoke('hotkeys_pause', { paused }).catch(() => {});
  }

  /** Gives the global hotkeys back and reads what is registered now. */
  async function resume() {
    await pause(false);
    try {
      v = await invoke<HotkeysView>('hotkeys_get');
    } catch {
      // The list stays as it was.
    }
  }

  function change(id: string) {
    if (editing) return;
    editing = id;
    pressed = '';
    error = '';
    pause(true);
    addEventListener('keydown', onKeyDown, true);
    addEventListener('keyup', onKeyUp, true);
    addEventListener('blur', onBlur);
  }

  function stopListening() {
    clearTimeout(sendTimer);
    removeEventListener('keydown', onKeyDown, true);
    removeEventListener('keyup', onKeyUp, true);
    removeEventListener('blur', onBlur);
  }

  /** Another window took the keyboard: Windows may have taken the keys too. */
  function onBlur() {
    if (pressed) send();
    else cancel();
  }

  function cancel() {
    if (!editing) return;
    turn += 1;
    stopListening();
    editing = '';
    pressed = '';
    resume();
  }

  function onKeyDown(e: KeyboardEvent) {
    // Nothing else on the page hears the keys: Escape must not leave the settings.
    e.preventDefault();
    e.stopImmediatePropagation();
    if (pressed) return;
    if (e.code === 'Escape') {
      cancel();
      return;
    }
    const combo = comboFromEvent(e);
    if (!combo) return;
    pressed = combo;
    // Registered while its keys are still down, a combination could fire at
    // once: it is sent when a key goes up, or soon if the page never hears that.
    sendTimer = setTimeout(send, RELEASE_WAIT_MS);
  }

  function onKeyUp(e: KeyboardEvent) {
    e.preventDefault();
    e.stopImmediatePropagation();
    if (pressed) send();
  }

  async function send() {
    if (!editing || !pressed) return;
    const [action, combo] = [editing, pressed];
    stopListening();
    const mine = turn;
    await run('hotkeys_set', { action, combo });
    // Cancelled meanwhile: the cancel gave the hotkeys back, and a new choice may have begun.
    if (turn !== mine) return;
    editing = '';
    pressed = '';
    // A refused combination leaves the hotkeys paused; an accepted one has resumed them already.
    resume();
  }

  onMount(() => {
    run('hotkeys_get', {});
    return () => {
      if (!editing) return;
      stopListening();
      pause(false);
    };
  });
</script>

{#if v}
  <div class="lbl">{t('hotkeys.list')}</div>
  <div class="rows">
    {#each v.hotkeys as row (row.id)}
      <div class="row" class:edit={editing === row.id}>
        <span class="what">
          {actionName(row.id)}
          {#if editing !== row.id && row.status === 'off'}
            <small>{t('hotkeys.status_off')}</small>
          {:else if editing !== row.id && row.status === 'needs'}
            <small>{needsName(row.id)}</small>
          {:else if editing !== row.id && row.status === 'taken'}
            <small class="taken">{t('hotkeys.status_taken')}</small>
          {/if}
        </span>
        {#if editing === row.id}
          <span class="press" role="status">{#if pressed}<kbd>{shown(pressed)}</kbd>{:else}{t('hotkeys.press')}{/if}</span>
          <button class="bt sm" onclick={cancel}>{t('hotkeys.cancel')}</button>
        {:else}
          <kbd class:off={row.status === 'off' || row.status === 'needs'} class:taken={row.status === 'taken'}>{row.combo}</kbd>
          <button class="bt sm" disabled={editing !== ''} onclick={() => change(row.id)}>{t('hotkeys.change')}</button>
          {#if row.custom}
            <button class="bt sm" disabled={editing !== ''} title={t('hotkeys.reset_title')} onclick={() => run('hotkeys_reset', { action: row.id })}>
              {t('hotkeys.reset')}
            </button>
          {/if}
        {/if}
      </div>
    {/each}
  </div>
  {#if error}<p class="err">{error}</p>{/if}
  <p class="hint">{t('hotkeys.hint')}</p>

  <div class="stack">
    <Switch
      checked={v.dictation}
      title={t('hotkeys.action.dictation')}
      sub={t('dictation.sub', { shortcut: comboOf('dictation') })}
      onchange={(value) => run('dictation_set_enabled', { value })}
    />
    {#if v.dictation && !v.microphone}<p class="warn">{t('dictation.needs_mic')}</p>{/if}
    {#if v.dictation && !v.stt_model}<p class="warn">{t('dictation.needs_stt')}</p>{/if}

    <Switch
      checked={v.screen_ask}
      title={t('hotkeys.action.screen_ask')}
      sub={t('screen_ask.sub', { shortcut: comboOf('screen_ask') })}
      onchange={(value) => run('screen_ask_set_enabled', { value })}
    />
    {#if v.screen_ask && !v.screen}<p class="warn">{t('screen_ask.needs_screen')}</p>{/if}
  </div>
{:else if error}
  <p class="err">{error}</p>
{/if}

<style>
  .rows {
    display: grid;
    gap: 2px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    min-height: 44px;
    padding: 4px 6px 4px 12px;
    border-radius: 12px;
    font-size: 13.5px;
  }
  .row:hover,
  .row.edit {
    background: rgba(255, 255, 255, 0.04);
  }
  .row.edit {
    outline: 1px solid rgba(143, 125, 255, 0.5);
  }
  .what {
    flex: 1 1 120px;
    min-width: 0;
    font-weight: 600;
  }
  /* Why the combination does not work now, under the name of the action. */
  small {
    display: block;
    font-size: 12px;
    font-weight: 500;
    color: var(--dim);
  }
  small.taken {
    color: var(--warn);
  }
  kbd {
    padding: 3px 9px;
    border-radius: 8px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.06);
    font: 600 12.5px/1.4 var(--mono);
    white-space: nowrap;
  }
  kbd.off {
    opacity: 0.45;
  }
  kbd.taken {
    border-color: var(--warn);
  }
  .press {
    font-size: 12.5px;
    color: var(--mute);
  }
  .bt.sm {
    height: 30px;
    padding: 0 12px;
    border-radius: 10px;
    font-size: 12.5px;
  }
  .hint {
    margin: 10px 2px 18px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .stack {
    display: grid;
    gap: 10px;
  }
  .warn {
    margin: -2px 2px 4px;
    font-size: 12.5px;
    color: var(--warn);
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
