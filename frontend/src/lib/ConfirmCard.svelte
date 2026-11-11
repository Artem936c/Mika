<script lang="ts">
  import { onMount } from 'svelte';
  import { RISK_LABELS, secondsLeft, type Pending } from './confirm';

  let {
    pending,
    onanswer,
    compact = false,
  }: { pending: Pending; onanswer: (a: 'once' | 'always' | 'deny') => void; compact?: boolean } = $props();

  let now = $state(Date.now());
  onMount(() => {
    const id = setInterval(() => (now = Date.now()), 250);
    return () => clearInterval(id);
  });
  const left = $derived(secondsLeft(pending, now));
  const level = $derived(pending.assessment.level);
</script>

<div class="cf lvl-{level}" class:compact>
  <div class="hd">
    <span class="badge">Риск: {RISK_LABELS[level]}</span>
    <span class="timer" title="Без ответа действие будет отклонено">{left} с</span>
  </div>
  <div class="what">{pending.assessment.summary}</div>
  <pre>{pending.assessment.target}</pre>
  <p class="why">{pending.assessment.reason}</p>
  <div class="row">
    <button class="bt pri" onclick={() => onanswer('once')}>Разрешить один раз</button>
    <button
      class="bt"
      disabled={!pending.allowAlways}
      title={pending.allowAlways
        ? 'Больше не спрашивать для этого действия'
        : level === 'high'
          ? 'Действие высокого риска нельзя разрешить навсегда'
          : 'В цели есть «*»: постоянное правило охватило бы лишнее'}
      onclick={() => onanswer('always')}>Разрешать всегда</button
    >
    <button class="bt no" onclick={() => onanswer('deny')}>Отклонить</button>
  </div>
  <p class="hint">
    Можно ответить в чате: «да»,
    {#if pending.allowAlways}«разреши всегда»,{/if}
    «нет».
  </p>
</div>

<style>
  .cf {
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    height: 100%;
    box-sizing: border-box;
    animation: in 0.35s var(--ease);
  }
  .cf.compact {
    height: auto;
    margin: 0 12px 10px;
    border-radius: 18px;
    border: 1px solid rgba(255, 198, 92, 0.3);
    background: rgba(255, 198, 92, 0.05);
  }
  .cf.compact.lvl-high {
    border-color: rgba(255, 107, 125, 0.4);
    background: rgba(255, 107, 125, 0.06);
  }
  @keyframes in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
  .hd {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  .badge {
    color: var(--warn);
  }
  .lvl-low .badge {
    color: var(--ok);
  }
  .lvl-high .badge {
    color: var(--bad);
  }
  .timer {
    color: var(--mute);
    font-variant-numeric: tabular-nums;
  }
  .what {
    font-size: 16px;
    font-weight: 700;
  }
  pre {
    margin: 0;
    font: 12.5px/1.45 'Cascadia Code', Consolas, monospace;
    color: #cdd3ff;
    background: rgba(0, 0, 0, 0.5);
    border: 1px solid var(--line);
    border-radius: 12px;
    padding: 8px 12px;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 72px;
    overflow: auto;
    user-select: text;
  }
  .why {
    margin: 2px 0 6px;
    font-size: 12.5px;
    color: var(--mute);
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .bt {
    height: 34px;
    padding: 0 14px;
    border-radius: 11px;
    font-size: 13px;
    font-weight: 700;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
    cursor: pointer;
  }
  .bt.pri {
    border: 0;
    background: linear-gradient(135deg, var(--v), #6b58e6);
    box-shadow: 0 6px 20px rgba(143, 125, 255, 0.35);
  }
  .bt.no {
    color: var(--bad);
  }
  .bt:disabled {
    opacity: 0.32;
    cursor: not-allowed;
  }
  .hint {
    margin: 4px 0 0;
    font-size: 11.5px;
    color: var(--dim);
  }
</style>
