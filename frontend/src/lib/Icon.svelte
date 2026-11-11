<script lang="ts">
  import { ICONS, type IconName } from './icons';

  let { name, size = 16 }: { name: IconName; size?: number } = $props();
</script>

<!-- The outlines are constants from icons.ts, never user text. -->
<i class="icon" style:width="{size}px" style:height="{size}px" aria-hidden="true">
  {@html `<svg viewBox="0 0 24 24">${ICONS[name]}</svg>`}
</i>

<style>
  .icon {
    display: inline-flex;
    flex: none;
  }
  .icon :global(svg) {
    width: 100%;
    height: 100%;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
