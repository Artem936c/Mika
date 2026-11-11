<script lang="ts">
  // Desktop avatar: a click-through window in the corner of the screen with a speech bubble.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import Avatar from './lib/Avatar.svelte';
  import AvatarDefs from './lib/AvatarDefs.svelte';
  import { isEmotion, reduceEmotion, type Emotion } from './lib/avatar';
  import { bubbleFor, lingerMs, type Bubble } from './lib/bubble';
  import { reducePending, type Pending } from './lib/confirm';
  import { t } from './lib/i18n';
  import { reduceState, type AssistantState } from './lib/island';

  let current = $state<AssistantState>('idle');
  let emotion = $state<Emotion>('neutral');
  let reply = $state('');
  let work = $state('');
  let pending = $state<Pending | null>(null);
  /** A finished reply stays for a few seconds after the state returns to idle. */
  let lingering = $state('');
  let lingerTimer: ReturnType<typeof setTimeout> | undefined;

  const live: Bubble = $derived(bubbleFor(current, reply, work, pending?.assessment.summary ?? null));
  const bubble: Bubble = $derived(live ?? (lingering ? { text: lingering } : null));
  const text = $derived(bubble === null ? '' : 'key' in bubble ? t(bubble.key) : bubble.text);
  // The words stay while the bubble fades out.
  let shownText = $state('');
  $effect(() => {
    if (text) shownText = text;
  });

  function onEvent(payload: unknown) {
    const before = current;
    current = reduceState(current, payload);
    emotion = reduceEmotion(emotion, payload);
    pending = reducePending(pending, payload, Date.now());
    const e = payload as { type?: string; text?: string; summary?: string };
    if (e?.type === 'user_text') {
      reply = '';
      lingering = '';
      clearTimeout(lingerTimer);
    }
    if (e?.type === 'assistant_delta' && typeof e.text === 'string') reply += e.text;
    if (e?.type === 'tool_started' && typeof e.summary === 'string') work = e.summary;
    if (e?.type === 'tool_finished') work = '';
    if (before !== 'idle' && current === 'idle' && reply) {
      const shown = bubbleFor('speaking', reply, '', null);
      lingering = shown && 'text' in shown ? shown.text : '';
      clearTimeout(lingerTimer);
      lingerTimer = setTimeout(() => (lingering = ''), lingerMs(lingering));
    }
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    invoke<AssistantState>('get_state').then((s) => (current = s));
    invoke<unknown>('get_emotion').then((e) => {
      if (isEmotion(e)) emotion = e;
    });
    listen('mika://event', (e) => onEvent(e.payload)).then((u) => (unlisten = u));
    return () => {
      unlisten?.();
      clearTimeout(lingerTimer);
    };
  });
</script>

<AvatarDefs />
<div class="stage">
  <div class="bbl" class:on={text !== ''}>{shownText}</div>
  <div class="cmp"><Avatar view="full" {emotion} talking={current === 'speaking'} /></div>
</div>

<style>
  .stage {
    position: absolute;
    inset: 0;
    overflow: hidden;
    pointer-events: none;
  }
  .cmp {
    position: absolute;
    right: 0;
    bottom: -6px;
    width: 300px;
    filter: drop-shadow(0 20px 40px rgba(0, 0, 0, 0.5));
    animation: rise 0.7s var(--spring) backwards;
  }
  @keyframes rise {
    from {
      translate: 0 110%;
      opacity: 0;
    }
  }
  .bbl {
    position: absolute;
    right: 262px;
    bottom: 236px;
    width: 250px;
    box-sizing: border-box;
    padding: 12px 15px;
    border-radius: 18px 18px 4px 18px;
    background: var(--glass);
    backdrop-filter: blur(16px);
    border: 1px solid var(--line2);
    font-size: 13.5px;
    line-height: 1.4;
    opacity: 0;
    scale: 0.9;
    transform-origin: 100% 100%;
    transition:
      opacity 0.35s,
      scale 0.35s var(--spring);
  }
  .bbl.on {
    opacity: 1;
    scale: 1;
  }
</style>
