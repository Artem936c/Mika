<script lang="ts">
  import { fade } from 'svelte/transition';
  import {
    BLINK,
    HEAD_CROP,
    MOTIONS,
    MOUTH_STEP_MS,
    POKE_MOTION,
    blinkPause,
    canBlink,
    canGesture,
    canTalk,
    frameUrl,
    gesturePause,
    idleGesture,
    nextMouth,
    type AvatarView,
    type Emotion,
    type EyeLayer,
    type Gesture,
    type Motion,
    type MouthLayer,
  } from './avatar';

  let {
    view = 'full',
    emotion = 'neutral',
    talking = false,
    size,
    onpoke,
  }: {
    view?: AvatarView;
    emotion?: Emotion;
    /** Moves the lips. */
    talking?: boolean;
    /** Round chip of this diameter, px. Without it the avatar fills its parent's width. */
    size?: number;
    /** A click on her; without it she is not clickable. */
    onpoke?: () => void;
  } = $props();

  let blinkEyes = $state<EyeLayer | null>(null);
  let gesture = $state<Gesture | null>(null);
  let mouth = $state<MouthLayer | null>(null);
  let mover: HTMLElement | undefined = $state();

  // A blink wins over a glance; lips while talking win over a quiet smile.
  const eyes = $derived(blinkEyes ?? gesture?.eyes ?? null);
  const shownMouth = $derived(talking ? mouth : (gesture?.mouth ?? null));

  const still = () => size !== undefined || window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;

  function play(m: Motion | undefined) {
    if (!m || !mover || still()) return;
    mover.animate(m.frames, { duration: m.ms, easing: 'ease-in-out' });
  }

  // Blinking: a short run of eye layers, then a random pause.
  $effect(() => {
    if (!canBlink(emotion)) {
      blinkEyes = null;
      return;
    }
    let timer: ReturnType<typeof setTimeout>;
    const blink = (step: number) => {
      const [layer, ms] = BLINK[step];
      blinkEyes = layer;
      timer = step + 1 < BLINK.length ? setTimeout(() => blink(step + 1), ms) : setTimeout(() => blink(0), blinkPause(Math.random()));
    };
    timer = setTimeout(() => blink(0), blinkPause(Math.random()));
    return () => clearTimeout(timer);
  });

  // Idle life: now and then she glances aside or smiles with her mouth closed.
  $effect(() => {
    if (!canGesture(emotion) || talking) {
      gesture = null;
      return;
    }
    let timer: ReturnType<typeof setTimeout>;
    const next = () => {
      gesture = idleGesture(Math.random());
      timer = setTimeout(() => {
        gesture = null;
        timer = setTimeout(next, gesturePause(Math.random()));
      }, gesture.ms);
    };
    timer = setTimeout(next, gesturePause(Math.random()));
    return () => clearTimeout(timer);
  });

  // Lips while she talks.
  $effect(() => {
    if (!talking || !canTalk(emotion)) {
      mouth = null;
      return;
    }
    const timer = setInterval(() => (mouth = nextMouth(mouth, Math.random())), MOUTH_STEP_MS);
    return () => clearInterval(timer);
  });

  // The body moves once when a new emotion begins.
  let shown: Emotion | null = null;
  $effect(() => {
    const e = emotion;
    if (shown !== null && shown !== e) play(MOTIONS[e]);
    shown = e;
  });

  function poke(e: MouseEvent) {
    if (!onpoke) return;
    e.stopPropagation();
    play(POKE_MOTION);
    onpoke();
  }

  // In the head chip the frame is enlarged and shifted so the face fills the circle.
  const figStyle = $derived(
    view === 'head'
      ? `width:${100 / HEAD_CROP.w}%;left:${(-HEAD_CROP.x / HEAD_CROP.w) * 100}%;top:${(-HEAD_CROP.y / HEAD_CROP.w) * (4 / 3) * 100}%`
      : '',
  );
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<span
  class="avatar"
  class:chip={size !== undefined}
  class:head={view === 'head'}
  class:talking
  class:pokable={onpoke !== undefined}
  style:width={size !== undefined ? `${size}px` : undefined}
  style:height={size !== undefined ? `${size}px` : undefined}
  onclick={poke}
>
  <span class="fig" style={figStyle}>
    <span class="mv" bind:this={mover}>
      {#key emotion}
        <img class="frame" src={frameUrl(emotion)} alt="" draggable="false" transition:fade={{ duration: size === undefined ? 220 : 0 }} />
      {/key}
      {#if eyes}<img class="layer" src={frameUrl(eyes)} alt="" draggable="false" />{/if}
      {#if shownMouth}<img class="layer" src={frameUrl(shownMouth)} alt="" draggable="false" />{/if}
    </span>
  </span>
</span>

<style>
  .avatar {
    display: block;
    position: relative;
  }
  .pokable {
    cursor: pointer;
  }
  .fig {
    display: block;
    position: relative;
    width: 100%;
    aspect-ratio: 3 / 4;
  }
  .mv {
    position: absolute;
    inset: 0;
    transform-origin: 50% 100%;
  }
  .head .fig {
    position: absolute;
  }
  .avatar:not(.head) {
    aspect-ratio: 3 / 4;
  }
  img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    user-select: none;
    pointer-events: none;
  }
  .chip {
    flex: none;
    border-radius: 50%;
    overflow: hidden;
    background: radial-gradient(circle at 50% 30%, #4a4090, #1b1736);
    box-shadow: 0 0 0 1.5px rgba(255, 255, 255, 0.14);
  }
</style>
