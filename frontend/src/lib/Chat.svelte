<script lang="ts">
  import { tick } from 'svelte';
  import Avatar from './Avatar.svelte';
  import { dragRegion, toggleMaximize } from './drag';
  import ConfirmCard from './ConfirmCard.svelte';
  import Icon from './Icon.svelte';
  import ImageCard from './ImageCard.svelte';
  import SelectionCard from './SelectionCard.svelte';
  import type { Selection } from './selection';
  import TaskPanel from './TaskPanel.svelte';
  import type { TaskView } from './task';
  import WeatherCard from './WeatherCard.svelte';
  import type { Emotion } from './avatar';
  import { formatSeconds, inlineMarkup, type ChatItem } from './chat';
  import type { Pending } from './confirm';
  import { t } from './i18n';
  import { AVATAR_COLUMN, stateLabel, type AssistantState } from './island';

  let {
    items,
    status,
    name,
    speak,
    screen,
    mic,
    pending,
    task = null,
    emotion,
    wide,
    windowed = false,
    needsSetup = false,
    onanswer,
    onsend,
    onstop,
    onclose,
    onsettings,
    onspeak,
    onscreen,
    onmic,
    onlisten,
    onwindowed,
    onnew,
    ontaskhide = () => {},
    onpoke,
    selection = null,
    onselection = () => {},
    onselectionpaste = async () => {},
    onselectionclose = () => {},
  }: {
    items: ChatItem[];
    status: AssistantState;
    name: string;
    speak: boolean;
    /** The user allowed looking at the screen. */
    screen: boolean;
    /** The user allowed the microphone. */
    mic: boolean;
    pending: Pending | null;
    /** Plan of the running multi-step task. */
    task?: TaskView | null;
    emotion: Emotion;
    /** Show the avatar column on the left. */
    wide: boolean;
    /** The chat lives in an ordinary window, not in the island. */
    windowed?: boolean;
    needsSetup?: boolean;
    onanswer: (a: 'once' | 'always' | 'deny') => void;
    onsend: (text: string) => void;
    onstop: () => void;
    onclose: () => void;
    onsettings: () => void;
    onspeak: (v: boolean) => void;
    onscreen: (v: boolean) => void;
    onmic: (v: boolean) => void;
    /** Start recording a phrase, or finish the running recording. */
    onlisten: () => void;
    /** Turn the island into an ordinary window, or back. */
    onwindowed: (v: boolean) => void;
    onnew: () => void;
    /** Hide a finished task panel. */
    ontaskhide?: () => void;
    /** A click on the avatar. */
    onpoke?: () => void;
    /** Text selected in another program, sent by the shortcut. */
    selection?: Selection | null;
    onselection?: (action: string) => void;
    onselectionpaste?: () => Promise<void>;
    onselectionclose?: () => void;
  } = $props();

  let draft = $state('');
  let list: HTMLDivElement | undefined = $state();
  let input: HTMLTextAreaElement | undefined = $state();
  const busy = $derived(status !== 'idle');

  $effect(() => {
    void items.length;
    void items[items.length - 1]?.text;
    void pending;
    tick().then(() => list?.scrollTo({ top: list.scrollHeight }));
  });

  $effect(() => {
    input?.focus();
  });

  // The field grows with the text up to four lines.
  $effect(() => {
    void draft;
    if (!input) return;
    input.style.height = 'auto';
    input.style.height = `${Math.min(input.scrollHeight, 84)}px`;
  });

  function submit() {
    const text = draft.trim();
    if (!text) return;
    onsend(text);
    draft = '';
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === 'Escape') {
      onclose();
    }
  }
</script>

<div class="chat" class:wide style:--avatar-column="{AVATAR_COLUMN}px">
  {#if wide}
    <div class="stagecol">
      <div class="halo"></div>
      <div class="girlbox"><Avatar view="full" {emotion} talking={status === 'speaking'} {onpoke} /></div>
      <div class="plate">
        <b>{name}</b>
        <span><i class="live" class:busy></i>{t(stateLabel(status)).toLowerCase()}</span>
      </div>
    </div>
  {/if}

  <div class="chatcol">
    <header class="cbar" use:dragRegion={{ ondouble: windowed ? toggleMaximize : undefined }} title={t('chat.drag_title')}>
      {#if !wide}
        <span title={t(stateLabel(status))}><Avatar view="head" {emotion} size={30} talking={status === 'speaking'} /></span>
      {/if}
      <button class="tg" class:on={speak} title={t('chat.speak_title')} aria-pressed={speak} onclick={() => onspeak(!speak)}>
        <Icon name={speak ? 'vol' : 'mute'} size={14} />{t('chat.speak')}
      </button>
      <button class="tg" class:on={mic} title={t('chat.mic_title')} aria-pressed={mic} onclick={() => onmic(!mic)}>
        <Icon name="mic" size={14} />{t('chat.mic')}
      </button>
      <button class="tg" class:on={screen} title={t('chat.screen_title')} aria-pressed={screen} onclick={() => onscreen(!screen)}>
        <Icon name="mon" size={14} />{t('chat.screen')}
      </button>
      <span class="grow"></span>
      <button class="ib" title={t('chat.new_title')} aria-label={t('chat.new')} onclick={onnew}><Icon name="plus" /></button>
      <button
        class="ib"
        class:on={windowed}
        title={t(windowed ? 'chat.pin_title' : 'chat.unpin_title')}
        aria-label={t(windowed ? 'chat.pin' : 'chat.unpin')}
        aria-pressed={windowed}
        onclick={() => onwindowed(!windowed)}
      >
        <Icon name="out" />
      </button>
      <button class="ib" title={t('chat.settings')} aria-label={t('chat.settings')} onclick={onsettings}><Icon name="set" /></button>
      {#if !windowed}
        <button class="ib" title={t('chat.close_title')} aria-label={t('chat.close')} onclick={onclose}><Icon name="x" /></button>
      {/if}
    </header>

    {#if needsSetup}
      <div class="setup">
        <span>{t('chat.setup', { name })}</span>
        <button class="bt pri" onclick={onsettings}>{t('chat.setup_button')}</button>
      </div>
    {/if}

    <div class="msgs" bind:this={list}>
      <div class="flow">
        {#if items.length === 0}
          <p class="empty">{t('chat.empty')}</p>
        {/if}
        {#each items as it, i (i)}
          {#if it.kind === 'user'}
            <div class="bub u">{it.text}</div>
          {:else if it.kind === 'assistant'}
            <!-- One line: the bubble keeps whitespace (pre-wrap). -->
            <div class="bub a" class:pending={it.pending}>{#each inlineMarkup(it.text) as s, j (j)}{#if s.kind === 'code'}<code>{s.text}</code>{:else if s.kind === 'bold'}<b>{s.text}</b>{:else}{s.text}{/if}{/each}</div>
          {:else if it.kind === 'tool'}
            <div class="stp" class:fail={it.ok === false} title={it.name}>
              {#if it.pending}
                <span class="orb"></span>
              {:else}
                <Icon name={it.ok === false ? 'x' : 'check'} size={13} />
              {/if}
              {#if it.data?.kind === 'volume'}
                <span>{t('chat.volume', { was: it.data.was, level: it.data.level })}</span>
                <span class="vol"><i style:width="{it.data.level}%"></i></span>
              {:else}
                <span class="what">{it.text}</span>
              {/if}
              {#if it.ms !== undefined}<span class="ms">· {t('chat.seconds', { value: formatSeconds(it.ms) })}</span>{/if}
            </div>
            {#if it.data?.kind === 'weather'}
              <WeatherCard data={it.data} />
            {:else if it.data?.kind === 'image'}
              <ImageCard path={it.data.path} />
            {/if}
          {:else if it.kind === 'timing'}
            {#if it.done && it.timings}
              <div class="tim">
                {#if it.timings.stt !== undefined}<span>{t('chat.time_stt', { value: formatSeconds(it.timings.stt) })}</span>{/if}
                {#if it.timings.llm !== undefined}<span>{t('chat.time_llm', { value: formatSeconds(it.timings.llm) })}</span>{/if}
                {#if it.timings.tts !== undefined}<span>{t('chat.time_tts', { value: formatSeconds(it.timings.tts) })}</span>{/if}
              </div>
            {/if}
          {:else if it.kind === 'reminder'}
            <div class="rem"><Icon name="bell" size={15} /><span>{it.text}</span></div>
          {:else}
            <div class="err"><Icon name="alert" size={14} /><span>{it.text}</span></div>
          {/if}
        {/each}
      </div>
    </div>

    {#if task}
      <TaskPanel {task} {onstop} onhide={ontaskhide} />
    {/if}
    {#if pending}
      <ConfirmCard {pending} {onanswer} compact />
    {/if}
    {#if selection}
      <SelectionCard {selection} {busy} onaction={onselection} onpaste={onselectionpaste} onclose={onselectionclose} />
    {/if}

    <div class="comp">
      <textarea
        bind:this={input}
        bind:value={draft}
        onkeydown={onkey}
        rows="1"
        placeholder={t('chat.placeholder')}
        aria-label={t('chat.message')}
      ></textarea>
      <button
        class="ib"
        class:rec={status === 'listening'}
        disabled={!mic}
        title={t(!mic ? 'chat.voice_off' : status === 'listening' ? 'chat.voice_stop' : 'chat.voice_input')}
        aria-label={t(status === 'listening' ? 'chat.voice_stop' : 'chat.voice_input')}
        aria-pressed={status === 'listening'}
        onclick={onlisten}
      >
        <Icon name="mic" />
      </button>
      {#if busy && !draft.trim()}
        <button class="ib stop" onclick={onstop} title={t('chat.stop')} aria-label={t('chat.stop')}><Icon name="stop" /></button>
      {:else}
        <button class="ib on" onclick={submit} disabled={!draft.trim()} title={t('chat.send_title')} aria-label={t('chat.send')}>
          <Icon name="up" />
        </button>
      {/if}
    </div>
  </div>
</div>

<style>
  .chat {
    display: grid;
    grid-template-columns: 1fr;
    width: 100%;
    height: 100%;
    text-align: left;
  }
  .chat.wide {
    grid-template-columns: var(--avatar-column) 1fr;
  }

  /* ---- avatar column ---- */
  .stagecol {
    position: relative;
    overflow: clip;
    border-right: 1px solid var(--line);
    background: linear-gradient(180deg, #151331, #0b0b16);
  }
  .girlbox {
    position: absolute;
    left: 50%;
    bottom: 40px;
    width: 270px;
    translate: -50% 0;
  }
  .plate {
    position: absolute;
    left: 14px;
    right: 14px;
    bottom: 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 9px 12px;
    border-radius: 14px;
    background: rgba(8, 9, 15, 0.66);
    backdrop-filter: blur(14px);
    border: 1px solid var(--line);
    font-size: 13px;
  }
  .plate b {
    font-weight: 800;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .plate span {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--mute);
    font-size: 12px;
    white-space: nowrap;
  }
  .live {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    box-shadow: 0 0 10px var(--ok);
  }
  .live.busy {
    background: var(--c);
    box-shadow: 0 0 10px var(--c);
  }

  /* ---- chat column ---- */
  .chatcol {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .cbar {
    height: 54px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 14px;
    border-bottom: 1px solid var(--line);
  }
  .grow {
    flex: 1;
  }
  .setup {
    margin: 10px 14px 0;
    padding: 10px 10px 10px 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    border-radius: 16px;
    border: 1px solid rgba(143, 125, 255, 0.35);
    background: rgba(143, 125, 255, 0.1);
    font-size: 13px;
  }
  .msgs {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 16px;
    display: flex;
    -webkit-mask: linear-gradient(transparent, #000 18px);
    mask: linear-gradient(transparent, #000 18px);
  }
  /* Messages stick to the bottom while the list is short. */
  .flow {
    margin-top: auto;
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 0 12px;
  }
  .empty {
    margin: 0 auto 150px;
    color: var(--dim);
    text-align: center;
  }
  .bub {
    max-width: 80%;
    padding: 9px 14px;
    border-radius: 18px;
    font-size: 14px;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .bub.u {
    align-self: flex-end;
    background: linear-gradient(135deg, var(--v), #6b58e6);
    border-bottom-right-radius: 6px;
    font-weight: 500;
  }
  .bub.a {
    align-self: flex-start;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid var(--line);
    border-bottom-left-radius: 6px;
    font-weight: 400;
  }
  .bub.a code {
    font-family: var(--mono);
    font-size: 0.92em;
    padding: 1px 5px;
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.08);
  }
  .bub.a.pending::after {
    content: '▍';
    color: var(--v);
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
  .stp {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11.5px;
    color: var(--mute);
    font-weight: 600;
  }
  .stp :global(.icon) {
    color: var(--ok);
  }
  .stp.fail :global(.icon) {
    color: var(--bad);
  }
  .stp .what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .stp .ms {
    flex: none;
    font-variant-numeric: tabular-nums;
  }
  .orb {
    flex: none;
    width: 11px;
    height: 11px;
    box-sizing: border-box;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.15);
    border-top-color: var(--c);
    animation: rot 0.8s linear infinite;
  }
  @keyframes rot {
    to {
      rotate: 360deg;
    }
  }
  .vol {
    flex: none;
    width: 90px;
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .vol i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--p));
  }
  .err {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 9px 12px;
    border-radius: 14px;
    font-size: 13px;
    color: #ffd0d6;
    background: rgba(255, 107, 125, 0.1);
    border: 1px solid rgba(255, 107, 125, 0.3);
    user-select: text;
  }
  .err :global(.icon) {
    color: var(--bad);
    margin-top: 2px;
  }
  .tim {
    display: flex;
    gap: 12px;
    margin: -3px 0 0 4px;
    font-size: 11px;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
  }
  .rem {
    align-self: flex-start;
    max-width: 80%;
    display: flex;
    gap: 9px;
    align-items: flex-start;
    padding: 9px 14px;
    border-radius: 16px;
    font-size: 14px;
    line-height: 1.4;
    /* The end of focus lists what came, one line each. */
    white-space: pre-wrap;
    word-break: break-word;
    background: rgba(111, 227, 255, 0.08);
    border: 1px solid rgba(111, 227, 255, 0.3);
    user-select: text;
  }
  .rem :global(.icon) {
    flex: none;
    color: var(--c);
    margin-top: 2px;
  }
  .comp {
    flex: none;
    margin: 0 14px 14px;
    min-height: 46px;
    box-sizing: border-box;
    border-radius: 23px;
    display: flex;
    align-items: flex-end;
    gap: 8px;
    padding: 5px 6px 5px 18px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid var(--line2);
  }
  .comp:focus-within {
    border-color: rgba(143, 125, 255, 0.55);
  }
  textarea {
    flex: 1;
    min-width: 0;
    align-self: center;
    border: 0;
    outline: 0;
    resize: none;
    background: transparent;
    font-size: 14px;
    line-height: 1.5;
    padding: 0;
    user-select: text;
  }
  textarea::placeholder {
    color: var(--dim);
  }
  .comp .ib {
    width: 34px;
    height: 34px;
  }
  .ib.rec {
    color: #fff;
    border-color: transparent;
    background: linear-gradient(135deg, var(--v), var(--p));
    animation: rec 1.1s ease-in-out infinite;
  }
  @keyframes rec {
    50% {
      box-shadow: 0 0 0 5px rgba(143, 125, 255, 0.22);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .ib.rec {
      animation: none;
    }
  }
  .ib.stop {
    color: var(--bad);
    border-color: rgba(255, 107, 125, 0.4);
    background: rgba(255, 107, 125, 0.12);
  }
</style>
