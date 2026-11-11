<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { IslandShape } from './island';

  let {
    shape,
    tone = 'none',
    onclick,
    children,
  }: { shape: IslandShape; tone?: 'none' | 'listen' | 'work' | 'warn' | 'danger'; onclick?: () => void; children: Snippet } = $props();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="island tone-{tone}"
  style:width="{shape.width}px"
  style:height="{shape.height}px"
  style:border-radius="{shape.radius}px"
  {onclick}
>
  {@render children()}
</div>

<style>
  .island {
    position: absolute;
    left: 50%;
    top: 14px;
    transform: translateX(-50%);
    box-sizing: border-box;
    background: var(--island);
    border: 1px solid rgba(255, 255, 255, 0.1);
    overflow: hidden;
    box-shadow: 0 14px 44px rgba(0, 0, 0, 0.55);
    transition:
      width 620ms var(--spring),
      height 620ms var(--spring),
      border-radius 620ms var(--spring),
      box-shadow 0.5s;
  }
  .tone-listen {
    box-shadow: 0 0 0 1px rgba(143, 125, 255, 0.5), 0 0 28px rgba(143, 125, 255, 0.35);
  }
  .tone-work {
    box-shadow: 0 0 0 1px rgba(111, 227, 255, 0.35), 0 14px 44px rgba(0, 0, 0, 0.55);
  }
  .tone-warn {
    box-shadow: 0 0 0 1px rgba(255, 198, 92, 0.45), 0 0 50px rgba(255, 198, 92, 0.12);
  }
  .tone-danger {
    box-shadow: 0 0 0 1px rgba(255, 107, 125, 0.55), 0 0 50px rgba(255, 107, 125, 0.16);
  }
</style>
