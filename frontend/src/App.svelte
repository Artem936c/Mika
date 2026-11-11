<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import Island from './lib/Island.svelte';
  import Face from './lib/Face.svelte';
  import Chat from './lib/Chat.svelte';
  import Settings from './lib/Settings.svelte';
  import ConfirmCard from './lib/ConfirmCard.svelte';
  import { reducePending, type Pending } from './lib/confirm';
  import { rectForShape, reduceState, unionRect, type AssistantState, type IslandShape } from './lib/island';
  import { fromHistory, reduceChat, viewShape, type ChatItem, type View } from './lib/chat';

  const CANVAS_WIDTH = 1000;
  const ORDER: AssistantState[] = ['idle', 'listening', 'thinking', 'speaking', 'awaiting_confirm'];

  interface UiSettings {
    name: string;
    speak_replies: boolean;
  }

  let current = $state<AssistantState>('idle');
  let view = $state<View>('island');
  let panel = $state<'chat' | 'settings'>('chat');
  let items = $state<ChatItem[]>([]);
  let name = $state('Мика');
  let speak = $state(true);
  let reply = $state('');
  let work = $state('');
  let pending = $state<Pending | null>(null);

  const shape: IslandShape = $derived(viewShape(view, current));
  const tone = $derived(
    pending
      ? pending.assessment.level === 'high'
        ? 'danger'
        : 'warn'
      : current === 'listening'
        ? 'listen'
        : current === 'thinking'
          ? 'work'
          : 'none',
  );

  function answer(a: 'once' | 'always' | 'deny') {
    if (pending) invoke('answer_confirm', { id: pending.id, answer: a });
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
    items = reduceChat(items, payload);
    pending = reducePending(pending, payload, Date.now());
    const e = payload as { type?: string; text?: string; summary?: string };
    if (e?.type === 'user_text') reply = '';
    if (e?.type === 'assistant_delta' && typeof e.text === 'string') reply += e.text;
    if (e?.type === 'tool_started' && typeof e.summary === 'string') work = e.summary;
    if (e?.type === 'tool_finished') work = '';
  }

  let needsSetup = $state(false);

  async function refreshSetup() {
    try {
      const s = await invoke<{ kind: string; has_key: boolean }>('get_llm_settings');
      needsSetup = !s.has_key && s.kind !== 'ollama';
    } catch {
      needsSetup = false;
    }
  }

  function openChat() {
    if (view === 'island') {
      view = 'chat';
      panel = needsSetup ? 'settings' : 'chat';
    }
    invoke('focus_island');
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    invoke<AssistantState>('get_state').then((s) => (current = s));
    invoke<unknown>('get_history').then((h) => (items = fromHistory(h)));
    invoke<{ id: number; tool: string; assessment: unknown; allow_always: boolean } | null>('get_pending_confirm').then((p) => {
      if (p) pending = reducePending(null, { type: 'confirm_requested', ...p, timeout_s: 60 }, Date.now());
    });
    invoke<UiSettings>('get_ui_settings').then((s) => {
      name = s.name;
      speak = s.speak_replies;
    });
    listen('mika://event', (e) => onEvent(e.payload)).then((u) => (unlisten = u));
    let unlistenUi: (() => void) | undefined;
    listen<{ action?: string }>('mika://ui', (e) => {
      if (e.payload?.action === 'open_chat') {
        view = 'chat';
        panel = needsSetup ? 'settings' : 'chat';
      }
    }).then((u) => (unlistenUi = u));
    refreshSetup();

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
</script>

<!-- The confirm card has its own buttons; a click on it must not open the chat. -->
<Island {shape} {tone} onclick={pending && view === 'island' ? undefined : openChat}>
  {#if view === 'chat'}
    {#if panel === 'settings'}
      <Settings
        onback={() => {
          panel = 'chat';
          refreshSetup();
        }}
      />
    {:else}
      <Chat
        {items}
        status={current}
        {name}
        {speak}
        {pending}
        {needsSetup}
        onanswer={answer}
        onsend={(text) => invoke('send_message', { text })}
        onstop={() => invoke('stop')}
        onclose={() => (view = 'island')}
        onsettings={() => (panel = 'settings')}
        onnew={async () => {
          await invoke('stop');
          await invoke('clear_history');
          items = [];
          reply = '';
        }}
        onspeak={(v) => {
          speak = v;
          invoke('set_speak_replies', { value: v });
        }}
      />
    {/if}
  {:else if pending}
    <ConfirmCard {pending} onanswer={answer} />
  {:else}
    <Face status={current} {name} {reply} {work} />
  {/if}
</Island>
