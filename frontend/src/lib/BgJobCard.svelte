<script lang="ts">
  // A background job in the chat: what it does, how far it is, the stop button and the outcome.
  // The card draws state only: every number and note comes from Rust, nothing is counted here.
  import { invoke } from '@tauri-apps/api/core';
  import AvFilesCard from './AvFilesCard.svelte';
  import Icon from './Icon.svelte';
  import { jobActive, jobStateKey, repeatsLabel, type JobView } from './bgjobs';
  import { cardData } from './chat';
  import { t } from './i18n';

  let { job }: { job: JobView } = $props();

  let error = $state('');
  const active = $derived(jobActive(job));
  const working = $derived(job.state === 'running' || job.state === 'stopping');
  const label = $derived(t(jobStateKey(job)));
  /** The outcome in words, once the job has ended and when it says more than the label. */
  const outcome = $derived(!active && job.text && !repeatsLabel(job.text, label) ? job.text : '');
  const files = $derived.by(() => {
    const card = cardData(job.data);
    return card?.kind === 'av_files' ? card.files : null;
  });

  async function stop() {
    try {
      await invoke('bg_job_stop', { id: job.id });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="job st-{job.state}" class:live={job.live}>
  <div class="head">
    <b class="title" title={job.title}>{job.title}</b>
    {#if active}
      <button
        class="bt"
        disabled={job.state === 'stopping'}
        title={t(job.live ? 'rec.stop_title' : 'bgjob.stop_title')}
        onclick={stop}
      >
        <Icon name="stop" size={12} />{t(job.live ? 'rec.stop' : 'bgjob.stop')}
      </button>
    {/if}
  </div>
  <div class="row">
    {#if job.live && active}<i class="dot" class:on={job.state === 'running'}></i>{/if}
    <span class="state">{label}</span>
    {#if job.live}
      <!-- The time, or why the recording is paused. While the file is being saved the label says it all. -->
      {#if job.state === 'running' && job.note}<span class="side" title={job.note}>{job.note}</span>{/if}
    {:else if working}
      <span class="bar" role="progressbar" aria-valuenow={job.share} aria-valuemin={0} aria-valuemax={100}><i style:width="{job.share}%"></i></span>
      <span class="pct">{job.share}%</span>
    {/if}
  </div>
  {#if active && !job.live && job.note}<p class="note" title={job.note}>{job.note}</p>{/if}
  {#if outcome}<p class="text" title={outcome}>{outcome}</p>{/if}
  {#if active && error}<p class="bad">{error}</p>{/if}
</div>
{#if files}<AvFilesCard {files} />{/if}

<style>
  .job {
    align-self: flex-start;
    width: 420px;
    max-width: 96%;
    box-sizing: border-box;
    padding: 10px 12px 10px 14px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.04);
    font-size: 13px;
    line-height: 1.4;
  }
  .job.live:not(.st-done, .st-failed, .st-stopped) {
    border-color: rgba(255, 59, 48, 0.4);
    background: rgba(255, 59, 48, 0.07);
  }
  .job.st-failed {
    border-color: rgba(255, 107, 125, 0.3);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .title {
    flex: 1;
    min-width: 0;
    font-weight: 700;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .head .bt {
    flex: none;
    height: 30px;
    padding: 0 11px;
    font-size: 12px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 9px;
    min-height: 20px;
    margin-top: 2px;
    font-size: 12px;
    color: var(--mute);
  }
  .state {
    flex: none;
    font-weight: 700;
    color: var(--c);
  }
  .st-queued .state,
  .st-stopped .state {
    color: var(--mute);
  }
  .st-done .state {
    color: var(--ok);
  }
  .st-failed .state {
    color: var(--bad);
  }
  .live:not(.st-done, .st-failed, .st-stopped) .state {
    color: #ff8a80;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: rgb(255, 59, 48);
    opacity: 0.55;
  }
  .dot.on {
    opacity: 1;
    animation: live 1.4s ease-in-out infinite;
  }
  @keyframes live {
    50% {
      opacity: 0.35;
    }
  }
  .side {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .bar {
    flex: 1;
    min-width: 40px;
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .bar i {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: linear-gradient(90deg, var(--v), var(--c));
    transition: width 0.3s;
  }
  .pct {
    flex: none;
    min-width: 32px;
    text-align: right;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  p {
    margin: 4px 0 0;
    user-select: text;
  }
  .note {
    font-size: 12px;
    color: var(--mute);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The outcome in words. A transcript can be long: the start is shown, the files hold the rest. */
  .text {
    font-size: 12.5px;
    color: var(--ink);
    white-space: pre-line;
    word-break: break-word;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .st-failed .text {
    color: #ffd0d6;
  }
  .bad {
    font-size: 12px;
    color: var(--bad);
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.on {
      animation: none;
    }
    .bar i {
      transition: none;
    }
  }
</style>
