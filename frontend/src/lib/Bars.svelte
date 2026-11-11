<script lang="ts">
  // Voice wave: bars that rise and fall out of step.
  let { count = 26 }: { count?: number } = $props();

  const bars = $derived(
    Array.from({ length: count }, () => ({
      duration: (0.45 + Math.random() * 0.7).toFixed(2),
      delay: Math.random().toFixed(2),
    })),
  );
</script>

<span class="bars" aria-hidden="true">
  {#each bars as b, i (i)}
    <u style:animation-duration="{b.duration}s" style:animation-delay="-{b.delay}s"></u>
  {/each}
</span>

<style>
  .bars {
    display: flex;
    align-items: center;
    gap: 3px;
    height: 26px;
    flex: 1;
  }
  u {
    flex: 1;
    border-radius: 2px;
    background: linear-gradient(var(--c), var(--v) 50%, var(--p));
    height: 20%;
    animation: bar 1s ease-in-out infinite alternate;
  }
  @keyframes bar {
    to {
      height: 100%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    u {
      animation: none;
      height: 55%;
    }
  }
</style>
