<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';
  import type { ScreenAsk } from './hotkeys';
  import { t } from './i18n';

  let {
    ask,
    busy,
    mic,
    listening,
    onclose = () => {},
  }: {
    /** The window whose picture waits for a question. */
    ask: ScreenAsk;
    /** She is answering or listening: the question waits. */
    busy: boolean;
    /** The user allowed the microphone. */
    mic: boolean;
    /** A phrase is being recorded now. */
    listening: boolean;
    /** The card is done: the picture was sent or thrown away. */
    onclose?: () => void;
  } = $props();

  let question = $state('');
  let preview = $state('');
  let previewError = $state('');
  let error = $state('');
  let field: HTMLInputElement | undefined = $state();

  // The picture itself stays in the app; the page asks for it by command.
  // A new picture starts the card anew: the old question was about another window.
  $effect(() => {
    void ask;
    let stale = false;
    preview = '';
    previewError = '';
    question = '';
    error = '';
    invoke<string>('screen_ask_preview')
      .then((src) => {
        if (!stale) preview = src;
      })
      .catch((e) => {
        if (!stale) previewError = String(e);
      });
    return () => {
      stale = true;
    };
  });

  // The question field takes the keyboard with every new picture, after the
  // chat has focused its own field.
  $effect(() => {
    void ask;
    const el = field;
    if (el) tick().then(() => el.focus());
  });

  async function send() {
    if (busy) return;
    error = '';
    try {
      await invoke('screen_ask_send', { question });
      onclose();
    } catch (e) {
      error = String(e);
    }
  }

  /** Starts a spoken question, or finishes the phrase being recorded. */
  async function say() {
    // While she answers, only a phrase being recorded can be finished.
    if (busy && !listening) return;
    error = '';
    try {
      if (await invoke<boolean>('screen_ask_listen')) onclose();
    } catch (e) {
      // An empty reason: the recording was cancelled.
      error = String(e ?? '');
    }
  }

  function close() {
    invoke('screen_ask_cancel').catch(() => {});
    onclose();
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.stopPropagation();
      close();
    }
  }
</script>

<div class="ask">
  <div class="hd">
    <Icon name="mon" size={14} />
    <span>{t('screen_ask.card_title', { app: ask.app || '—' })}</span>
    <button class="x" title={t('screen_ask.close')} aria-label={t('screen_ask.close')} onclick={close}><Icon name="x" size={13} /></button>
  </div>
  <div class="body">
    {#if preview}
      <img src={preview} alt={t('screen_ask.preview_alt', { title: ask.title })} title={ask.title} />
    {:else if previewError}
      <p class="note bad shot">{previewError}</p>
    {/if}
    <div class="form">
      <input
        class="fld"
        bind:this={field}
        bind:value={question}
        onkeydown={onkey}
        maxlength="500"
        placeholder={t('screen_ask.placeholder')}
        aria-label={t('screen_ask.question')}
      />
      <div class="line">
        <p class="note">{t('screen_ask.sends')}</p>
        <button
          class="ib"
          class:rec={listening}
          disabled={!mic || (busy && !listening)}
          title={t(!mic ? 'screen_ask.say_off' : listening ? 'screen_ask.say_stop' : 'screen_ask.say')}
          aria-label={t(listening ? 'screen_ask.say_stop' : 'screen_ask.say')}
          aria-pressed={listening}
          onclick={say}
        >
          <Icon name="mic" />
        </button>
        <button class="bt pri" disabled={busy} onclick={send}>{t('screen_ask.ask')}</button>
      </div>
      {#if error}<p class="note bad">{error}</p>{/if}
    </div>
  </div>
</div>

<style>
  .ask {
    flex: none;
    margin: 0 14px 10px;
    padding: 12px 14px;
    border-radius: 20px;
    background: var(--glass);
    border: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .hd {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--mute);
  }
  .hd span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    margin-left: auto;
    background: none;
    border: 0;
    color: var(--mute);
    cursor: pointer;
    padding: 2px;
    display: flex;
  }
  .body {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }
  img {
    flex: none;
    max-width: 40%;
    max-height: 120px;
    object-fit: contain;
    border-radius: 10px;
    border: 1px solid var(--line2);
  }
  .shot {
    flex: 0 1 40%;
  }
  .form {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  /* What leaves the computer is said beside the buttons that send it. */
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .line .note {
    flex: 1;
    min-width: 0;
  }
  .bt,
  .ib {
    flex: none;
  }
  .ib {
    width: 36px;
    height: 36px;
  }
  /* As the microphone button of the chat while it records. */
  .ib.rec {
    color: #fff;
    border-color: transparent;
    background: linear-gradient(135deg, var(--v), var(--p));
    animation: rec 1.1s ease-in-out infinite;
  }
  @keyframes rec {
    50% {
      box-shadow: 0 0 0 5px rgba(var(--v-rgb), 0.22);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .ib.rec {
      animation: none;
    }
  }
  .note {
    margin: 0;
    font-size: 12px;
    color: var(--mute);
  }
  .note.bad {
    color: var(--bad);
    user-select: text;
    word-break: break-word;
  }
</style>
