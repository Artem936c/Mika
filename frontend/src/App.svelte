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
  import { fromHistory, reduceChat, type ChatItem } from './lib/chat';
  import { reducePending, type Pending } from './lib/confirm';
  import { reduceTask, type TaskView } from './lib/task';
  import {
    ISLAND_SHAPES,
    opensChat,
    rectForShape,
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

  /** A fired reminder stays in the island this long, like a spoken reply. */
  const NOTICE_MS = 9000;

  let current = $state<AssistantState>('idle');
  let view = $state<View>('island');
  let settingsStart = $state<'persona' | 'providers'>('persona');
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
  /** Text of a reminder that has just fired. */
  let notice = $state('');
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  let emotion = $state<Emotion>('neutral');
  let reply = $state('');
  let work = $state('');
  let tool = $state('');
  let pending = $state<Pending | null>(null);
  let task = $state<TaskView | null>(null);
  let needsSetup = $state(false);

  // The state can say "awaiting" a moment before the question itself arrives.
  const faceState: AssistantState = $derived(
    current === 'awaiting_confirm' ? 'thinking' : current === 'idle' && notice ? 'speaking' : current,
  );
  const shape: IslandShape = $derived(
    view === 'island'
      ? pending
        ? ISLAND_SHAPES.awaiting_confirm
        : ISLAND_SHAPES[faceState]
      : viewShape(view, current, avatarInIsland),
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
    emotion = reduceEmotion(emotion, payload);
    items = reduceChat(items, payload);
    pending = reducePending(pending, payload, Date.now());
    task = reduceTask(task, payload);
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
    if (e?.type === 'reminder_fired' && typeof e.text === 'string') {
      const task = (payload as { kind?: string }).kind === 'task';
      if (!task) {
        notice = e.text;
        clearTimeout(noticeTimer);
        noticeTimer = setTimeout(() => (notice = ''), NOTICE_MS);
      }
      loadIdle();
    }
    if (e?.type === 'user_text') notice = '';
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
    invoke<AssistantState>('get_state').then((s) => (current = s));
    invoke<unknown>('get_emotion').then((e) => {
      if (isEmotion(e)) emotion = e;
    });
    invoke<unknown>('get_history').then((h) => (items = fromHistory(h)));
    invoke<{ id: number; tool: string; assessment: unknown; allow_always: boolean } | null>('get_pending_confirm').then((p) => {
      if (p) pending = reducePending(null, { type: 'confirm_requested', ...p, timeout_s: 60 }, Date.now());
    });
    loadUi();
    refreshSetup();
    loadIdle();
    // Often enough to follow the player; the weather behind it is cached for half an hour.
    const idleTimer = setInterval(loadIdle, 15_000);
    listen('mika://event', (e) => onEvent(e.payload)).then((u) => (unlisten = u));
    listen<{ action?: string }>('mika://ui', (e) => {
      if (e.payload?.action === 'open_chat') openPanel();
      if (e.payload?.action === 'settings_changed') loadUi();
    }).then((u) => (unlistenUi = u));

    const onKey = (e: KeyboardEvent) => {
      if (!import.meta.env.DEV || view !== 'island') return;
      const i = Number(e.key) - 1;
      if (i >= 0 && i < ORDER.length) invoke('debug_set_state', { next: ORDER[i] });
    };
    addEventListener('keydown', onKey);
    return () => {
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
    onanswer={answer}
    onsend={(text) => invoke('send_message', { text })}
    onstop={() => invoke('stop')}
    onclose={() => {
      if (!windowed) view = 'island';
    }}
    onsettings={() => {
      settingsStart = needsSetup ? 'providers' : 'persona';
      view = 'settings';
    }}
    onnew={async () => {
      await invoke('stop');
      await invoke('clear_history');
      items = [];
      reply = '';
      task = null;
    }}
    ontaskhide={() => (task = null)}
    onpoke={() => invoke('poke_avatar')}
    onspeak={setSpeak}
    onscreen={setScreen}
    onmic={setMic}
    onlisten={() => invoke('voice_listen')}
    onwindowed={setWindowed}
  />
{/snippet}

{#if windowed}
  <div class="win">
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
<Island {shape} {tone} movable={view === 'island'} onclick={onIslandClick}>
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
  {:else if pending}
    <div class="layer" style={size(ISLAND_SHAPES.awaiting_confirm)} in:layerIn out:layerOut>
      <ConfirmCard {pending} onanswer={answer} />
    </div>
  {:else}
    {#key faceState}
      <div class="layer" style={size(ISLAND_SHAPES[faceState])} in:layerIn out:layerOut>
        <Face
          status={faceState}
          {name}
          reply={current === 'idle' && notice ? notice : reply}
          {work}
          {tool}
          {emotion}
          {speak}
          weather={idle.weather}
          timer={idle.reminder}
          track={idle.track}
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
