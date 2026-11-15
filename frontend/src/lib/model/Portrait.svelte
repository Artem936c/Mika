<script lang="ts">
  // A chip of a model avatar: a picture drawn at import, one per emotion,
  // with the mouth shut and open. No scene: a chip is 30 to 56 pixels.
  import { MOUTH_STEP_MS, type Emotion } from '../avatar';
  import { stillName } from './show';
  import { model } from './state.svelte';

  let { emotion, talking = false }: { emotion: Emotion; talking?: boolean } = $props();

  let open = $state(false);

  $effect(() => {
    if (!talking) {
      open = false;
      return;
    }
    const timer = setInterval(() => (open = !open), MOUTH_STEP_MS * 2);
    return () => clearInterval(timer);
  });

  // A model without the open-mouth picture keeps its mouth shut.
  const src = $derived(model.stills.get(stillName(emotion, open)) ?? model.stills.get(stillName(emotion, false)) ?? '');
</script>

{#if src}<img class="portrait" {src} alt="" draggable="false" />{/if}

<style>
  .portrait {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
    user-select: none;
    pointer-events: none;
  }
</style>
