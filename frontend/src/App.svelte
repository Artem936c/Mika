<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import AvatarDefs from './lib/AvatarDefs.svelte';
  import Chat from './lib/Chat.svelte';
  import ConfirmCard from './lib/ConfirmCard.svelte';
  import Face from './lib/Face.svelte';
  import Island from './lib/Island.svelte';
  import Settings from './lib/Settings.svelte';
  import { isEmotion, reduceEmotion, type Emotion } from './lib/avatar';
  import { fromHistory, reduceChat, type ChatItem } from './lib/chat';
  import { reducePending, type Pending } from './lib/confirm';
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
  import { layerIn, layerOut } from './lib/layer';

  const CANVAS_WIDTH = 1000;
  const ORDER: AssistantState[] = ['idle', 'listening', 'thinking', 'speaking', 'awaiting_confirm'];

  interface UiSettings {
    name: string;
    speak_replies: boolean;
    avatar_in_island: boolean;
  }

  let current = $state<AssistantState>('idle');
  let view = $state<View>('island');
  let settingsStart = $state<'persona' | 'providers'>('persona');
  let items = $state<ChatItem[]>([]);
  let name = $state('');
  let speak = $state(true);
  let avatarInIsland = $state(true);
  let emotion = $state<Emotion>('neutral');
  let reply = $state('');
  let work = $state('');
  let tool = $state('');
  let pending = $state<Pending | null>(null);
  let needsSetup = $state(false);

  // The state can say "awaiting" a moment before the question itself arrives.
  const faceState: AssistantState = $derived(current === 'awaiting_confirm' ? 'thinking' : current);
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
    }
  }

  async function loadUi() {
    const s = await invoke<UiSettings>('get_ui_settings');
    name = s.name;
    speak = s.speak_replies;
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
      unlisten?.();
      unlistenUi?.();
      removeEventListener('keydown', onKey);
    };
  });

  const size = (s: IslandShape) => `width:${s.width}px;height:${s.height}px`;
</script>

<AvatarDefs />
<Island {shape} {tone} onclick={onIslandClick}>
  {#if view === 'settings'}
    <div class="layer" style={size(shape)} in:layerIn out:layerOut>
      <Settings
        {name}
        {emotion}
        start={settingsStart}
        onback={() => {
          view = 'chat';
          refreshSetup();
        }}
      />
    </div>
  {:else if view === 'chat'}
    <div class="layer" style={size(shape)} in:layerIn out:layerOut>
      <Chat
        {items}
        status={current}
        {name}
        {speak}
        {pending}
        {emotion}
        wide={avatarInIsland}
        {needsSetup}
        onanswer={answer}
        onsend={(text) => invoke('send_message', { text })}
        onstop={() => invoke('stop')}
        onclose={() => (view = 'island')}
        onsettings={() => {
          settingsStart = needsSetup ? 'providers' : 'persona';
          view = 'settings';
        }}
        onnew={async () => {
          await invoke('stop');
          await invoke('clear_history');
          items = [];
          reply = '';
        }}
        onspeak={setSpeak}
      />
    </div>
  {:else if pending}
    <div class="layer" style={size(ISLAND_SHAPES.awaiting_confirm)} in:layerIn out:layerOut>
      <ConfirmCard {pending} onanswer={answer} />
    </div>
  {:else}
    {#key faceState}
      <div class="layer" style={size(ISLAND_SHAPES[faceState])} in:layerIn out:layerOut>
        <Face status={faceState} {name} {reply} {work} {tool} {emotion} {speak} onspeak={setSpeak} />
      </div>
    {/key}
  {/if}
</Island>

<style>
  /* Each form keeps its final size while the island springs to it. */
  .layer {
    position: absolute;
    top: 0;
    left: 50%;
    translate: -50% 0;
  }
</style>
