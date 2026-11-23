<script lang="ts">
  import type { Snippet } from 'svelte';
  import { dragRegion } from './drag';
  import { t } from './i18n';
  import type { IslandShape } from './island';

  let {
    shape,
    tone = 'none',
    movable = true,
    recording = false,
    onclick,
    children,
  }: {
    shape: IslandShape;
    /** The whole island moves only while folded; an open panel moves by its top bar. */
    movable?: boolean;
    /** `glow`: listening or thinking. `warn`: a question is open. `danger`: the question is about a high-risk action. */
    tone?: 'none' | 'glow' | 'warn' | 'danger';
    /** The screen is being recorded: a red dot on the edge, whatever the island shows. */
    recording?: boolean;
    onclick?: (e: MouseEvent) => void;
    children: Snippet;
  } = $props();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="iw tone-{tone}"
  style:width="{shape.width}px"
  style:height="{shape.height}px"
  style:border-radius="{shape.radius}px"
  {onclick}
  use:dragRegion={{ threshold: true, disabled: !movable }}
>
  <div class="ring"></div>
  <div class="island">
    {@render children()}
  </div>
  {#if recording}
    <span class="rec" role="img" title={t('rec.island_title')} aria-label={t('rec.island_title')}></span>
  {/if}
</div>

<style>
  .iw {
    position: absolute;
    top: 14px;
    left: 50%;
    translate: -50% 0;
    transition:
      width 620ms var(--spring),
      height 620ms var(--spring),
      border-radius 620ms var(--spring),
      box-shadow 0.5s;
    box-shadow: 0 14px 44px rgba(0, 0, 0, 0.55);
  }
  /* Gradient outline that turns around the island. */
  .ring {
    position: absolute;
    inset: -1.5px;
    border-radius: inherit;
    padding: 1.5px;
    background: conic-gradient(from var(--a), var(--v), var(--p), var(--c), var(--v));
    -webkit-mask:
      linear-gradient(#000 0 0) content-box,
      linear-gradient(#000 0 0);
    -webkit-mask-composite: xor;
    mask:
      linear-gradient(#000 0 0) content-box,
      linear-gradient(#000 0 0);
    mask-composite: exclude;
    opacity: 0;
    transition: opacity 0.45s;
    animation: spin 3.2s linear infinite;
    pointer-events: none;
  }
  @keyframes spin {
    to {
      --a: 360deg;
    }
  }
  .tone-glow {
    box-shadow:
      0 14px 44px rgba(0, 0, 0, 0.55),
      0 0 46px rgba(var(--v-rgb), 0.42);
  }
  .tone-glow .ring {
    opacity: 1;
  }
  .tone-warn {
    box-shadow:
      0 14px 44px rgba(0, 0, 0, 0.55),
      0 0 0 1px rgba(255, 198, 92, 0.4),
      0 0 50px rgba(255, 198, 92, 0.12);
  }
  .tone-danger {
    box-shadow:
      0 14px 44px rgba(0, 0, 0, 0.55),
      0 0 60px rgba(255, 107, 125, 0.38);
  }
  .tone-danger .ring {
    opacity: 1;
    background: conic-gradient(from var(--a), var(--bad), var(--hot), var(--bad));
  }
  .island {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    /* clip, not hidden: focus or scrollIntoView must never scroll the island itself. */
    overflow: clip;
    background: var(--island);
    border: 1px solid rgba(255, 255, 255, 0.1);
  }
  .island::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.12);
  }
  /* Screen recording: sits on the top edge near the right corner, over any content. */
  .rec {
    position: absolute;
    top: -4px;
    right: 24px;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: rgb(255, 59, 48);
    box-shadow:
      0 0 0 2px var(--island),
      0 0 10px rgba(255, 59, 48, 0.8);
    animation: rec 1.4s ease-in-out infinite;
  }
  @keyframes rec {
    50% {
      opacity: 0.45;
      scale: 0.8;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .ring,
    .rec {
      animation: none;
    }
  }
</style>
