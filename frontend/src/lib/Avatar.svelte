<script lang="ts">
  import { fade } from 'svelte/transition';
  import {
    BLINK,
    HEAD_CROP,
    MOUTH_STEP_MS,
    blinkPause,
    canBlink,
    canTalk,
    frameUrl,
    nextMouth,
    type AvatarView,
    type Emotion,
    type EyeLayer,
    type MouthLayer,
  } from './avatar';

  let {
    view = 'full',
    emotion = 'neutral',
    talking = false,
    size,
  }: {
    view?: AvatarView;
    emotion?: Emotion;
    /** Moves the lips. */
    talking?: boolean;
    /** Round chip of this diameter, px. Without it the avatar fills its parent's width. */
    size?: number;
  } = $props();

  let eyes = $state<EyeLayer | null>(null);
  let mouth = $state<MouthLayer | null>(null);

  // Blinking: a short run of eye layers, then a random pause.
  $effect(() => {
    if (!canBlink(emotion)) {
      eyes = null;
      return;
    }
    let timer: ReturnType<typeof setTimeout>;
    const blink = (step: number) => {
      const [layer, ms] = BLINK[step];
      eyes = layer;
      timer = step + 1 < BLINK.length ? setTimeout(() => blink(step + 1), ms) : setTimeout(() => blink(0), blinkPause(Math.random()));
    };
    timer = setTimeout(() => blink(0), blinkPause(Math.random()));
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

  // In the head chip the frame is enlarged and shifted so the face fills the circle.
  const figStyle = $derived(
    view === 'head'
      ? `width:${100 / HEAD_CROP.w}%;left:${(-HEAD_CROP.x / HEAD_CROP.w) * 100}%;top:${(-HEAD_CROP.y / HEAD_CROP.w) * (4 / 3) * 100}%`
      : '',
  );
</script>

<span
  class="avatar"
  class:chip={size !== undefined}
  class:head={view === 'head'}
  class:talking
  style:width={size !== undefined ? `${size}px` : undefined}
  style:height={size !== undefined ? `${size}px` : undefined}
>
  <span class="fig" style={figStyle}>
    {#key emotion}
      <img class="frame" src={frameUrl(emotion)} alt="" draggable="false" transition:fade={{ duration: size === undefined ? 220 : 0 }} />
    {/key}
    {#if eyes}<img class="layer" src={frameUrl(eyes)} alt="" draggable="false" />{/if}
    {#if mouth}<img class="layer" src={frameUrl(mouth)} alt="" draggable="false" />{/if}
  </span>
</span>

<style>
  .avatar {
    display: block;
    position: relative;
  }
  .fig {
    display: block;
    position: relative;
    width: 100%;
    aspect-ratio: 3 / 4;
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
