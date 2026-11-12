<script lang="ts">
  // Plan of the running task: steps with their state, progress and a stop button.
  import Icon from './Icon.svelte';
  import { t } from './i18n';
  import { progress, type TaskView } from './task';

  let { task, onstop, onhide }: { task: TaskView; onstop: () => void; onhide: () => void } = $props();

  const p = $derived(progress(task));
</script>

<div class="task" class:fin={task.finished} class:bad={task.finished && !task.ok}>
  <div class="head">
    <b title={task.title}>{task.title}</b>
    <span class="cnt">
      {#if task.finished}{t(task.ok ? 'task.done' : 'task.failed')} · {/if}{t('task.progress', { done: p.done, total: p.total })}
    </span>
    <span class="bar"><i style:width="{p.total ? (p.done / p.total) * 100 : 0}%"></i></span>
    {#if task.finished}
      <button class="ib sm" title={t('task.hide')} aria-label={t('task.hide')} onclick={onhide}><Icon name="x" size={13} /></button>
    {:else}
      <button class="ib sm stop" title={t('task.stop')} aria-label={t('task.stop')} onclick={onstop}><Icon name="stop" size={13} /></button>
    {/if}
  </div>
  <ol>
    {#each task.steps as s, i (i)}
      <li class={s.status} title={t(`task.status.${s.status}`)}>
        <span class="mk">
          {#if s.status === 'active'}<span class="orb"></span>
          {:else if s.status === 'done'}<Icon name="check" size={12} />
          {:else if s.status === 'failed'}<Icon name="x" size={12} />
          {:else}<i class="dot"></i>{/if}
        </span>
        <span class="tx">{s.text}{#if s.note}<em> · {s.note}</em>{/if}</span>
      </li>
    {/each}
  </ol>
</div>

<style>
  .task {
    flex: none;
    margin: 0 14px 10px;
    padding: 10px 12px;
    border-radius: 16px;
    border: 1px solid rgba(111, 227, 255, 0.28);
    background: rgba(111, 227, 255, 0.06);
    font-size: 12.5px;
  }
  .task.fin {
    border-color: rgba(61, 220, 132, 0.3);
    background: rgba(61, 220, 132, 0.05);
  }
  .task.bad {
    border-color: rgba(255, 107, 125, 0.3);
    background: rgba(255, 107, 125, 0.06);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .head b {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    font-weight: 700;
  }
  .cnt {
    flex: none;
    color: var(--mute);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    flex: 1;
    min-width: 30px;
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.1);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--c), var(--v));
    transition: width 0.3s;
  }
  .ib.sm {
    flex: none;
    width: 26px;
    height: 26px;
  }
  .ib.stop {
    color: var(--bad);
    border-color: rgba(255, 107, 125, 0.4);
    background: rgba(255, 107, 125, 0.12);
  }
  ol {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    max-height: 132px;
    overflow-y: auto;
    display: grid;
    gap: 4px;
  }
  li {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    color: var(--mute);
    line-height: 1.35;
  }
  li.active {
    color: #fff;
    font-weight: 600;
  }
  li.done {
    color: var(--dim);
  }
  li.skipped .tx {
    text-decoration: line-through;
    color: var(--dim);
  }
  li.failed {
    color: #ffd0d6;
  }
  .mk {
    flex: none;
    width: 14px;
    height: 17px;
    display: grid;
    place-items: center;
  }
  li.done .mk :global(.icon) {
    color: var(--ok);
  }
  li.failed .mk :global(.icon) {
    color: var(--bad);
  }
  .dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.3);
  }
  .orb {
    width: 11px;
    height: 11px;
    box-sizing: border-box;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.15);
    border-top-color: var(--c);
    animation: rot 0.8s linear infinite;
  }
  @keyframes rot {
    to {
      rotate: 360deg;
    }
  }
  .tx {
    min-width: 0;
    word-break: break-word;
    user-select: text;
  }
  em {
    font-style: normal;
    font-weight: 400;
    color: var(--dim);
  }
  @media (prefers-reduced-motion: reduce) {
    .orb {
      animation: none;
    }
  }
</style>
