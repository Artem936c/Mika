<script lang="ts">
  // Desktop avatar: a window in the corner of the screen with a speech bubble.
  // Clicks on her are hers; every other click goes through (decision 0040).
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import Avatar from './lib/Avatar.svelte';
  import { isEmotion, reduceEmotion, type Emotion } from './lib/avatar';
  import { bubbleFor, lingerMs, type Bubble } from './lib/bubble';
  import { reducePending, type Pending } from './lib/confirm';
  import { t } from './lib/i18n';
  import { noticeOf, reduceState, type AssistantState } from './lib/island';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { model, startModelState } from './lib/model/state.svelte';
  import { live as signals, locate, probeAt, pushScene, react, startLive } from './lib/model/live.svelte';
  import { isOnFigure, wheelDue } from './lib/model/touch';
  import { startVoice, talkingNow } from './lib/model/voice.svelte';

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
    const noticed = noticeOf(payload);
    if (noticed !== null) {
      lingering = noticed;
      clearTimeout(lingerTimer);
      lingerTimer = setTimeout(() => (lingering = ''), lingerMs(lingering));
    }
    if (before !== 'idle' && current === 'idle' && reply) {
      const shown = bubbleFor('speaking', reply, '', null);
      lingering = shown && 'text' in shown ? shown.text : '';
      clearTimeout(lingerTimer);
      lingerTimer = setTimeout(() => (lingering = ''), lingerMs(lingering));
    }
  }

  let fig: HTMLElement | undefined = $state();
  /** The cursor is over her: the window takes clicks. */
  let over = false;

  /** Tells Rust whether the cursor is over her, when that changes. */
  function setOver(now: boolean) {
    if (now === over) return;
    over = now;
    invoke('avatar_hover', { over }).catch(() => {});
  }

  /**
   * Whether the cursor is on her: the opacity of a living model under it,
   * read from her next frame, or the middle of a picture.
   */
  function check(x: number, y: number) {
    if (probeAt(x, y, (alpha) => setOver(isOnFigure(alpha)))) return;
    if (!fig) return setOver(false);
    const r = fig.getBoundingClientRect();
    const side = r.width / 8;
    setOver(x >= r.left + side && x <= r.right - side && y >= r.top && y <= r.bottom);
  }

  // Checked when the cursor moves, and again now and then while it rests:
  // she moves under a resting cursor too.
  $effect(() => {
    const c = signals.cursor;
    if (!c) return;
    check(c.x, c.y);
    const again = setInterval(() => check(c.x, c.y), 150);
    return () => clearInterval(again);
  });

  /** A press on her: a short one is a click, one that moves drags the window. */
  let pressed: { x: number; y: number } | null = null;

  function onpointerdown(e: PointerEvent) {
    if (e.button === 0) pressed = { x: e.screenX, y: e.screenY };
  }

  function onpointermove(e: PointerEvent) {
    if (pressed && Math.hypot(e.screenX - pressed.x, e.screenY - pressed.y) > 4) {
      pressed = null;
      // Where the user leaves her is kept: only a move by the user.
      invoke('avatar_dragging').catch(() => {});
      try {
        getCurrentWindow()
          .startDragging()
          .catch(() => {});
      } catch {
        // Outside Tauri there is no window to drag.
      }
    }
  }

  function onpointerup(e: PointerEvent) {
    if (!pressed) return;
    pressed = null;
    const part = locate(e.clientX, e.clientY);
    // The emotion core picks her face by her character; her body follows that face (decision 0055).
    invoke<Emotion>('poke_avatar', { part })
      .then((face) => {
        if (part) react(part, isEmotion(face) ? face : undefined);
      })
      .catch(() => {
        if (part && part !== 'other') react(part);
      });
  }

  /** When the size last stepped under the wheel: a touchpad sends a flood. */
  let wheelAt = -Infinity;

  function onwheel(e: WheelEvent) {
    const now = performance.now();
    if (e.deltaY === 0 || !wheelDue(wheelAt, now)) return;
    wheelAt = now;
    invoke('avatar_scale_step', { delta: -e.deltaY }).catch(() => {});
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    // The cursor is needed here always: to tell clicks on her from clicks past her.
    invoke('avatar_cursor_wanted', { on: true }).catch(() => {});
    // Her body and hair lag behind a window on the move.
    let last: { x: number; y: number; t: number } | null = null;
    let stopMoved: (() => void) | undefined;
    try {
      getCurrentWindow()
        .onMoved(({ payload }) => {
        const t = performance.now();
        const k = window.devicePixelRatio || 1;
        if (last && t > last.t) {
          const ms = t - last.t;
          pushScene((((payload.x - last.x) / k) * 1000) / ms, (((payload.y - last.y) / k) * 1000) / ms);
        }
        last = { x: payload.x, y: payload.y, t };
        })
        .then((u) => (stopMoved = u))
        .catch(() => {});
    } catch {
      // Outside Tauri the window does not move.
    }
    startModelState();
    startVoice();
    startLive();
    invoke<AssistantState>('get_state').then((s) => (current = s));
    invoke<unknown>('get_emotion').then((e) => {
      if (isEmotion(e)) emotion = e;
    });
    listen('mika://event', (e) => onEvent(e.payload)).then((u) => (unlisten = u));
    // Shown again: Rust starts with clicks going through, the page with the cursor off her.
    let stopShown: (() => void) | undefined;
    listen<{ action?: string; label?: string; shown?: boolean }>('mika://ui', (e) => {
      if (e.payload?.action === 'window_shown' && e.payload.label === 'avatar') over = false;
    })
      .then((u) => (stopShown = u))
      .catch(() => {});
    return () => {
      unlisten?.();
      stopShown?.();
      stopMoved?.();
      clearTimeout(lingerTimer);
    };
  });
</script>

<!-- The window grows with the size from the settings; what is in it grows the same. -->
<div class="stage" style:zoom={model.look.scale}>
  <div class="bbl" class:on={text !== ''}>{shownText}</div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="cmp" bind:this={fig} {onpointerdown} {onpointermove} {onpointerup} {onwheel}>
    <Avatar view="full" {emotion} talking={talkingNow(current === 'speaking')} />
  </div>
</div>

<style>
  .stage {
    position: absolute;
    inset: 0;
    overflow: clip;
    pointer-events: none;
  }
  .cmp {
    position: absolute;
    right: 0;
    bottom: -6px;
    width: 300px;
    pointer-events: auto;
    touch-action: none;
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
