<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import Chat from './lib/Chat.svelte';
  import ConfirmCard from './lib/ConfirmCard.svelte';
  import Face from './lib/Face.svelte';
  import Island from './lib/Island.svelte';
  import Settings from './lib/Settings.svelte';
  import Welcome from './lib/Welcome.svelte';
  import { isEmotion, reduceEmotion, type Emotion } from './lib/avatar';
  import { listJobs, recordingOf, reduceJobsSeen, reduceRecording, type JobsSeen } from './lib/bgjobs';
  import { addJobs, fromHistory, reduceChat, refreshJobs, takeOffer, type ChatItem } from './lib/chat';
  import { reducePending, showsWhole, untranslated, type Pending } from './lib/confirm';
  import { selectionFrom, type Selection } from './lib/selection';
  import {
    dictationFrom,
    hotkeyNoticeOf,
    screenAskFrom,
    screenAskGone,
    screenAskLands,
    showsNotice,
    type DictationView,
    type ScreenAsk,
  } from './lib/hotkeys';
  import { t } from './lib/i18n';
  import { startModelState } from './lib/model/state.svelte';
  import { startLive } from './lib/model/live.svelte';
  import { startVoice } from './lib/model/voice.svelte';
  import { crashedOf } from './lib/journal';
  import { lang } from './lib/locale.svelte';
  import { reduceTask, type TaskView } from './lib/task';
  import {
    FOLD_AFTER_MS,
    ISLAND_SHAPES,
    confirmShape,
    foldsAway,
    noticeOf,
    opensChat,
    rectForShape,
    reducePaused,
    reduceSleeping,
    reduceState,
    unionRect,
    viewShape,
    type AssistantState,
    type IslandShape,
    type View,
  } from './lib/island';
  import { EDGES, resizeFrom } from './lib/drag';
  import { layerIn, layerOut } from './lib/layer';

  const CANVAS_WIDTH = 1000;
  const ORDER: AssistantState[] = ['idle', 'listening', 'thinking', 'speaking', 'awaiting_confirm'];

  interface UiSettings {
    name: string;
    speak_replies: boolean;
    screen_enabled: boolean;
    microphone_enabled: boolean;
    avatar_in_island: boolean;
    windowed: boolean;
    onboarded: boolean;
  }

  /** Below this width the ordinary window has no room for the avatar column. */
  const WIDE_FROM = 780;

  interface IdleInfo {
    weather: { temp: number; text: string } | null;
    reminder: { due_at: number; text: string } | null;
    track: { title: string; artist: string } | null;
  }

  /** A fired reminder or her own line stays in the island this long, like a spoken reply. */
  const NOTICE_MS = 9000;

  let current = $state<AssistantState>('idle');
  /** The pause after a full stop: she starts nothing until called (decision 0045). */
  let paused = $state(false);
  /** She sleeps: the pause she was asked for (decision 0050). */
  let sleeping = $state(false);
  let view = $state<View>('island');
  let settingsStart = $state<'persona' | 'providers' | 'journal'>('persona');
  let items = $state<ChatItem[]>([]);
  let name = $state('');
  let speak = $state(true);
  let screen = $state(false);
  let mic = $state(false);
  /** The main window is an ordinary window, not the island. */
  let windowed = $state(false);
  let innerWidth = $state(1000);
  let avatarInIsland = $state(true);
  let idle = $state<IdleInfo>({ weather: null, reminder: null, track: null });
  /** Text of a reminder that has just fired, or of her own line. */
  let notice = $state('');
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  let emotion = $state<Emotion>('neutral');
  let reply = $state('');
  let work = $state('');
  let tool = $state('');
  let pending = $state<Pending | null>(null);
  let selection = $state<Selection | null>(null);
  let dictation = $state<DictationView | null>(null);
  let screenAsk = $state<ScreenAsk | null>(null);
  /** The notice answers a hotkey: it is shown at once, also while she is busy. */
  let noticeUrgent = $state(false);
  const noticing = $derived(showsNotice(current, notice, noticeUrgent));
  let task = $state<TaskView | null>(null);
  let needsSetup = $state(false);
  /** The previous run was cut short and the user has not been told yet. */
  let crashed = $state(false);
  /** Id of the screen recording that runs now. */
  let recording = $state<number | null>(null);
  /** Newest state of every background job the events told about. */
  let jobsSeen: JobsSeen = {};
  /** The unsent text of the chat, kept while the island is folded. */
  let draft = $state('');
  /** The window lost focus since the chat opened and has not got it back. */
  let away = $state(false);

  // A new panel starts with the user at it, whatever the focus was before.
  $effect(() => {
    void view;
    away = false;
  });

  // The user went elsewhere and nothing goes on: the chat folds into the island.
  $effect(() => {
    const folds = foldsAway({
      view,
      windowed,
      away,
      state: current,
      asking: pending !== null || screenAsk !== null || dictation !== null,
    });
    if (!folds) return;
    const timer = setTimeout(() => (view = 'island'), FOLD_AFTER_MS);
    return () => clearTimeout(timer);
  });

  // The state can say "awaiting" a moment before the question itself arrives.
  const faceState: AssistantState = $derived(
    current === 'awaiting_confirm' ? 'thinking' : noticing ? 'speaking' : current,
  );
  // The island with the open question: as tall as its target needs, and by one
  // more line when the card says that the description is still in Russian.
  const asking: IslandShape | null = $derived(
    pending ? confirmShape(pending.assessment.target, showsWhole(pending.tool), untranslated(pending, lang())) : null,
  );
  const shape: IslandShape = $derived(
    view === 'island' ? (asking ?? ISLAND_SHAPES[faceState]) : viewShape(view, current, avatarInIsland),
  );
  const tone = $derived(
    pending
      ? pending.assessment.level === 'high'
        ? 'danger'
        : 'warn'
      : view === 'island' && (current === 'listening' || current === 'thinking')
        ? 'glow'
        : 'none',
  );

  function answer(a: 'once' | 'always' | 'deny') {
    if (pending) invoke('answer_confirm', { id: pending.id, answer: a });
  }

  function setSpeak(v: boolean) {
    speak = v;
    invoke('set_speak_replies', { value: v });
  }

  function setScreen(v: boolean) {
    screen = v;
    invoke('set_screen_enabled', { value: v });
  }

  function setMic(v: boolean) {
    mic = v;
    invoke('set_microphone_enabled', { value: v });
  }

  /** Island to ordinary window and back. */
  async function setWindowed(v: boolean) {
    try {
      await invoke('set_window_mode', { windowed: v });
      applyMode(v);
    } catch {
      // The window stays as it was.
    }
  }

  function applyMode(v: boolean) {
    if (v === windowed) return;
    windowed = v;
    // The ordinary window always shows the chat; the island starts folded.
    view = v ? 'chat' : 'island';
  }

  async function loadIdle() {
    try {
      idle = await invoke<IdleInfo>('idle_info');
    } catch {
      // The idle line is decoration: without it the island still works.
    }
  }

  /** Cards for the background jobs that still run: the chat is drawn anew at start and after «new chat». */
  /** Another conversation is open (decision 0052): the chat draws its history and the cards of running jobs. */
  async function showConversation() {
    reply = '';
    task = null;
    screenAsk = null;
    invoke('screen_ask_cancel');
    try {
      items = fromHistory(await invoke<unknown>('get_history'));
    } catch (e) {
      items = reduceChat([], { type: 'error', message: String(e) });
    }
    await loadJobs();
  }

  async function loadJobs() {
    try {
      const list = listJobs(await invoke<unknown>('bg_jobs_list'), jobsSeen);
      items = addJobs(items, list);
      // What the events said since is newer than the list.
      if (recording === null) recording = recordingOf(list);
    } catch {
      // Without the list the cards are missing; the jobs themselves go on.
    }
  }

  // Click-through rectangle follows the shape: union during the transition, then the new one (decision 0001).
  let lastShape: IslandShape | null = null;
  let settleTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const next = shape;
    const nextRect = rectForShape(next, CANVAS_WIDTH);
    clearTimeout(settleTimer);
    const value = lastShape ? unionRect(rectForShape(lastShape, CANVAS_WIDTH), nextRect) : nextRect;
    invoke('set_island_rect', { value });
    settleTimer = setTimeout(() => invoke('set_island_rect', { value: nextRect }), 700);
    lastShape = next;
  });

  function onEvent(payload: unknown) {
    current = reduceState(current, payload);
    paused = reducePaused(paused, payload);
    sleeping = reduceSleeping(sleeping, payload);
    emotion = reduceEmotion(emotion, payload);
    items = reduceChat(items, payload);
    pending = reducePending(pending, payload, Date.now());
    task = reduceTask(task, payload);
    recording = reduceRecording(recording, payload);
    // A job card comes with the answer of its tool, and an event of the job can be here before it.
    jobsSeen = reduceJobsSeen(jobsSeen, payload);
    items = refreshJobs(items, jobsSeen);
    const e = payload as { type?: string; text?: string; summary?: string; name?: string };
    if (e?.type === 'user_text') reply = '';
    if (e?.type === 'assistant_delta' && typeof e.text === 'string') reply += e.text;
    if (e?.type === 'tool_started' && typeof e.summary === 'string') {
      work = e.summary;
      tool = typeof e.name === 'string' ? e.name : '';
    }
    if (e?.type === 'tool_finished') {
      work = '';
      tool = '';
      if (e.name?.startsWith('reminder_')) loadIdle();
    }
    const noticed = noticeOf(payload);
    if (noticed !== null) {
      notice = noticed;
      noticeUrgent = hotkeyNoticeOf(payload) !== null;
      clearTimeout(noticeTimer);
      noticeTimer = setTimeout(() => (notice = ''), NOTICE_MS);
    }
    if (e?.type === 'reminder_fired') loadIdle();
    if (e?.type === 'user_text') notice = '';
    // A dictation never outlives the state it ran in, whatever became of its last message.
    if (e?.type === 'state_changed' && current === 'idle') dictation = null;
  }

  async function loadUi() {
    const s = await invoke<UiSettings>('get_ui_settings');
    name = s.name;
    speak = s.speak_replies;
    screen = s.screen_enabled;
    mic = s.microphone_enabled;
    applyMode(s.windowed === true);
    // First run: the welcome screen opens by itself and takes the keyboard.
    if (s.onboarded === false && view === 'island') {
      view = 'welcome';
      invoke('focus_island');
    }
    avatarInIsland = s.avatar_in_island;
  }

  async function refreshSetup() {
    try {
      const s = await invoke<{ kind: string; has_key: boolean }>('get_llm_settings');
      needsSetup = !s.has_key && s.kind !== 'ollama';
    } catch {
      needsSetup = false;
    }
  }

  async function loadCrash() {
    try {
      crashed = crashedOf(await invoke<unknown>('journal_view'));
    } catch {
      // The notice is a courtesy: the journal page itself shows what went wrong.
      crashed = false;
    }
  }

  /** The notice is read: the journal page opens and tells the app so. */
  function openJournal() {
    crashed = false;
    settingsStart = 'journal';
    view = 'settings';
  }

  function closeCrash() {
    crashed = false;
    invoke('journal_crash_seen').catch(() => {});
  }

  function openPanel() {
    if (view !== 'island') return;
    settingsStart = needsSetup ? 'providers' : 'persona';
    view = needsSetup ? 'settings' : 'chat';
  }

  function onIslandClick(e: MouseEvent) {
    if (!opensChat(e.target instanceof Element ? e.target : null)) return;
    // The confirm card has its own buttons; a click on it must not open the chat.
    if (!pending) openPanel();
    invoke('focus_island');
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let unlistenUi: (() => void) | undefined;
    startModelState();
    startVoice();
    startLive();
    invoke<AssistantState>('get_state').then((s) => (current = s));
    invoke<boolean>('get_paused')
      .then((p) => (paused = p === true))
      .catch(() => {});
    invoke<boolean>('get_sleeping')
      .then((p) => (sleeping = p === true))
      .catch(() => {});
    invoke<unknown>('get_emotion').then((e) => {
      if (isEmotion(e)) emotion = e;
    });
    invoke<unknown>('get_history')
      .then((h) => (items = fromHistory(h)))
      .then(loadJobs);
    invoke<{ id: number; tool: string; assessment: unknown; allow_always: boolean } | null>('get_pending_confirm').then((p) => {
      if (p) pending = reducePending(null, { type: 'confirm_requested', ...p, timeout_s: 60 }, Date.now());
    });
    loadUi();
    refreshSetup();
    loadCrash();
    loadIdle();
    // Often enough to follow the player; the weather behind it is cached for half an hour.
    const idleTimer = setInterval(loadIdle, 15_000);
    listen('mika://event', (e) => onEvent(e.payload)).then((u) => (unlisten = u));
    listen<{ action?: string }>('mika://ui', (e) => {
      if (e.payload?.action === 'open_chat') openPanel();
      const picked = selectionFrom(e.payload);
      if (picked) {
        selection = picked;
        openPanel();
      }
      const dictated = dictationFrom(e.payload);
      if (dictated !== undefined) dictation = dictated;
      const asked = screenAskFrom(e.payload);
      if (asked && screenAskLands(view, needsSetup) === 'chat') {
        screenAsk = asked;
        // From the settings too: the card lives in the chat.
        view = 'chat';
      } else if (asked) {
        // No chat to show the card in: the picture is not kept waiting unseen.
        screenAsk = null;
        invoke('screen_ask_cancel');
        notice = t('screen_ask.err_setup');
        noticeUrgent = true;
        clearTimeout(noticeTimer);
        noticeTimer = setTimeout(() => (notice = ''), NOTICE_MS);
      }
      // The app threw the picture away (five minutes, the screen switch or the action off).
      if (screenAskGone(e.payload)) screenAsk = null;
      if (e.payload?.action === 'settings_changed') loadUi();
    }).then((u) => (unlistenUi = u));

    const onKey = (e: KeyboardEvent) => {
      if (!import.meta.env.DEV || view !== 'island') return;
      const i = Number(e.key) - 1;
      if (i >= 0 && i < ORDER.length) invoke('debug_set_state', { next: ORDER[i] });
    };
    addEventListener('keydown', onKey);
    const onBlur = () => (away = true);
    const onFocus = () => (away = false);
    addEventListener('blur', onBlur);
    addEventListener('focus', onFocus);
    return () => {
      removeEventListener('blur', onBlur);
      removeEventListener('focus', onFocus);
      clearInterval(idleTimer);
      clearTimeout(noticeTimer);
      unlisten?.();
      unlistenUi?.();
      removeEventListener('keydown', onKey);
    };
  });

  const size = (s: IslandShape) => `width:${s.width}px;height:${s.height}px`;
</script>

<svelte:window bind:innerWidth />

{#snippet settingsPanel()}
  <Settings
    {name}
    {emotion}
    start={settingsStart}
    onback={() => {
      view = 'chat';
      refreshSetup();
      loadUi();
      loadIdle();
      // The journal page may have shown the notice itself.
      if (crashed) loadCrash();
    }}
  />
{/snippet}

{#snippet welcomePanel()}
  <Welcome
    {name}
    ondone={() => {
      view = 'chat';
      refreshSetup();
      loadUi();
    }}
  />
{/snippet}

{#snippet chatPanel()}
  <Chat
    {items}
    bind:draft
    status={current}
    {name}
    {speak}
    {screen}
    {mic}
    {pending}
    {task}
    {emotion}
    {windowed}
    wide={avatarInIsland && (!windowed || innerWidth >= WIDE_FROM)}
    {needsSetup}
    {crashed}
    onjournal={openJournal}
    oncrashclose={closeCrash}
    onanswer={answer}
    onsend={(text) => invoke('send_message', { text })}
    onskill={(id) => {
      items = takeOffer(items, id);
      invoke('skill_offer_accept', { id }).catch((e) => (items = reduceChat(items, { type: 'error', message: String(e) })));
    }}
    onstop={() => invoke('stop')}
    onclose={() => {
      if (!windowed) view = 'island';
    }}
    onsettings={() => {
      settingsStart = needsSetup ? 'providers' : 'persona';
      view = 'settings';
    }}
    onnew={async () => {
      // A new chat stops the reply and the task, without the pause; the old
      // conversation stays in the list (decision 0052).
      try {
        await invoke('new_conversation');
      } catch (e) {
        items = reduceChat(items, { type: 'error', message: String(e) });
        return;
      }
      await showConversation();
    }}
    onswitched={showConversation}
    ontaskhide={() => (task = null)}
    onpoke={(part) => invoke<string>('poke_avatar', { part })}
    onspeak={setSpeak}
    onscreen={setScreen}
    onmic={setMic}
    onlisten={() => invoke('voice_listen')}
    onwindowed={setWindowed}
    {selection}
    onselection={(action) => invoke('selection_action', { action })}
    onselectionpaste={() => invoke('selection_paste')}
    onselectionclose={() => {
      selection = null;
      invoke('selection_clear');
    }}
    {screenAsk}
    onscreenaskclose={() => (screenAsk = null)}
  />
{/snippet}

{#if windowed}
  <div class="win">
    {#if recording !== null}
      <!-- Without the island its red dot is drawn on the edge of the window, over whatever it shows. -->
      <span class="winrec" role="img" title={t('rec.island_title')} aria-label={t('rec.island_title')}></span>
    {/if}
    {#if view === 'welcome'}
      {@render welcomePanel()}
    {:else if view === 'settings'}
      {@render settingsPanel()}
    {:else}
      {@render chatPanel()}
    {/if}
  </div>
  <!-- No Windows frame: the edges of the page resize the window. -->
  {#each EDGES as edge (edge)}
    <div class="edge {edge}" aria-hidden="true" onpointerdown={(e) => resizeFrom(edge, e)}></div>
  {/each}
{:else}
<Island {shape} {tone} movable={view === 'island'} recording={recording !== null} onclick={onIslandClick}>
  {#if view === 'welcome'}
    <div class="layer" style={size(shape)} in:layerIn out:layerOut>
      {@render welcomePanel()}
    </div>
  {:else if view === 'settings'}
    <div class="layer" style={size(shape)} in:layerIn out:layerOut>
      {@render settingsPanel()}
    </div>
  {:else if view === 'chat'}
    <div class="layer" style={size(shape)} in:layerIn out:layerOut>
      {@render chatPanel()}
    </div>
  {:else if pending && asking}
    <div class="layer" style={size(asking)} in:layerIn out:layerOut>
      <ConfirmCard {pending} onanswer={answer} />
    </div>
  {:else}
    {#key faceState}
      <div class="layer" style={size(ISLAND_SHAPES[faceState])} in:layerIn out:layerOut>
        <Face
          status={faceState}
          {name}
          reply={noticing ? notice : reply}
          {work}
          {tool}
          {emotion}
          {speak}
          weather={idle.weather}
          timer={idle.reminder}
          track={idle.track}
          dictation={dictation?.phase ?? null}
          {paused}
          {sleeping}
          onspeak={setSpeak}
        />
      </div>
    {/key}
  {/if}
</Island>
{/if}

<style>
  /* Each form keeps its final size while the island springs to it. */
  /* Ordinary window: the chat or the settings fill it. The window itself is
     transparent and frameless, so the page draws the rounded edge. */
  .win {
    position: fixed;
    inset: 0;
    overflow: clip;
    border-radius: 14px;
    border: 1px solid var(--line2);
    background: var(--island);
  }
  /* Screen recording in the ordinary window: the dot of the island, on the top edge. */
  .winrec {
    position: absolute;
    z-index: 5;
    top: 5px;
    left: 50%;
    width: 9px;
    height: 9px;
    margin-left: -4.5px;
    border-radius: 50%;
    background: rgb(255, 59, 48);
    box-shadow:
      0 0 0 2px var(--island),
      0 0 10px rgba(255, 59, 48, 0.8);
    animation: winrec 1.4s ease-in-out infinite;
  }
  @keyframes winrec {
    50% {
      opacity: 0.45;
      scale: 0.8;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .winrec {
      animation: none;
    }
  }
  .edge {
    position: fixed;
    z-index: 10;
  }
  .edge.North,
  .edge.South {
    left: 10px;
    right: 10px;
    height: 5px;
    cursor: ns-resize;
  }
  .edge.North {
    top: 0;
  }
  .edge.South {
    bottom: 0;
  }
  .edge.East,
  .edge.West {
    top: 10px;
    bottom: 10px;
    width: 5px;
    cursor: ew-resize;
  }
  .edge.East {
    right: 0;
  }
  .edge.West {
    left: 0;
  }
  .edge.NorthEast,
  .edge.NorthWest,
  .edge.SouthEast,
  .edge.SouthWest {
    width: 12px;
    height: 12px;
  }
  .edge.NorthWest {
    top: 0;
    left: 0;
    cursor: nwse-resize;
  }
  .edge.SouthEast {
    bottom: 0;
    right: 0;
    cursor: nwse-resize;
  }
  .edge.NorthEast {
    top: 0;
    right: 0;
    cursor: nesw-resize;
  }
  .edge.SouthWest {
    bottom: 0;
    left: 0;
    cursor: nesw-resize;
  }
  .layer {
    position: absolute;
    top: 0;
    left: 50%;
    translate: -50% 0;
  }
</style>
