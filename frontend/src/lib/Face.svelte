<script lang="ts">
  import { onMount } from 'svelte';
  import { STATE_LABELS, type AssistantState } from './island';
  import { tailWords } from './chat';

  let { status, name, reply, work }: { status: AssistantState; name: string; reply: string; work: string } = $props();

  let clock = $state('');
  onMount(() => {
    const t = () => (clock = new Date().toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' }));
    t();
    const id = setInterval(t, 10_000);
    return () => clearInterval(id);
  });
</script>

{#key status}
  <div class="face">
    {#if status === 'idle'}
      <span class="orb"></span>
      <b>{name}</b>
      <span class="sep"></span>
      <span class="mute">{clock}</span>
    {:else if status === 'listening'}
      <span class="bars"><u></u><u></u><u></u><u></u><u></u></span>
      <span>{STATE_LABELS.listening}</span>
    {:else if status === 'thinking'}
      <span class="spin"></span>
      <span class="work">{work || STATE_LABELS.thinking}</span>
    {:else if status === 'speaking'}
      <span class="orb"></span>
      <span class="say">{tailWords(reply, 70) || STATE_LABELS.speaking}</span>
    {:else}
      <span class="warn">{STATE_LABELS.awaiting_confirm}</span>
    {/if}
  </div>
{/key}

<style>
  .face {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    padding: 0 18px;
    white-space: nowrap;
    cursor: pointer;
    animation: in 0.3s var(--ease);
  }
  @keyframes in {
    from {
      opacity: 0;
      filter: blur(6px);
    }
  }
  .orb {
    width: 22px;
    height: 22px;
    flex: none;
    border-radius: 50%;
    background: radial-gradient(circle at 35% 30%, #fff8, transparent 40%), linear-gradient(135deg, var(--v), var(--p));
  }
  b {
    font-weight: 700;
  }
  .sep {
    width: 1px;
    height: 14px;
    background: var(--line2);
  }
  .mute {
    color: var(--mute);
  }
  .work {
    color: var(--c);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .say {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 14px;
  }
  .warn {
    color: var(--warn);
    font-weight: 700;
  }
  .spin {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.15);
    border-top-color: var(--c);
    animation: rot 0.8s linear infinite;
  }
  @keyframes rot {
    to {
      transform: rotate(360deg);
    }
  }
  .bars {
    display: flex;
    gap: 3px;
    height: 18px;
    align-items: center;
  }
  .bars u {
    width: 3px;
    height: 40%;
    border-radius: 2px;
    background: linear-gradient(var(--c), var(--v));
    animation: bar 0.9s ease-in-out infinite alternate;
  }
  .bars u:nth-child(2) {
    animation-delay: 0.15s;
  }
  .bars u:nth-child(3) {
    animation-delay: 0.3s;
  }
  .bars u:nth-child(4) {
    animation-delay: 0.45s;
  }
  @keyframes bar {
    to {
      height: 100%;
    }
  }
</style>
