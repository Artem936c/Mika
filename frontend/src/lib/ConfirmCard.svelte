<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { alwaysLabel, alwaysTitle, confirmHead, secondsLeft, showsWhole, untranslated, type Pending } from './confirm';
  import { t } from './i18n';
  import { CONFIRM_LINES, confirmTargetHeight } from './island';
  import { lang } from './locale.svelte';

  let {
    pending,
    onanswer,
    compact = false,
  }: {
    pending: Pending;
    onanswer: (a: 'once' | 'always' | 'deny') => void;
    /** Card inside the chat: it takes its own height and has a frame. */
    compact?: boolean;
  } = $props();

  let now = $state(Date.now());
  onMount(() => {
    const id = setInterval(() => (now = Date.now()), 250);
    return () => clearInterval(id);
  });
  const left = $derived(secondsLeft(pending, now));
  const level = $derived(pending.assessment.level);
  // A target written in lines is shown whole, as far as the card has room: on the
  // island `confirmShape` makes that room, in the chat the card is as tall as it needs.
  const listed = $derived(pending.assessment.target.includes('\n'));
  // A scenario by event is shown whole by its own rule (`pre.whole`, 11 lines):
  // the eight-line box of a listed target must not cut it.
  const whole = $derived(showsWhole(pending.tool));
</script>

<div class="cf lvl-{level}" class:compact>
  <div class="hd">
    <Icon name={level === 'high' ? 'shield' : 'term'} />
    <span>{t(confirmHead(level))}</span>
    <span class="timer" title={t('confirm.timer_title')}>{t('chat.seconds', { value: left })}</span>
  </div>
  {#if untranslated(pending, lang())}<p class="untr">{t('confirm.untranslated')}</p>{/if}
  <h3>{pending.assessment.summary}</h3>
  <pre class:whole style:max-height={listed && !whole ? `${confirmTargetHeight(CONFIRM_LINES)}px` : undefined}>{pending.assessment.target}</pre>
  <p class="why">{pending.assessment.reason}</p>
  <div class="acts">
    <button class="bt pri" onclick={() => onanswer('once')}><Icon name="check" />{t('confirm.once')}</button>
    <button class="bt" disabled={!pending.allowAlways} title={t(alwaysTitle(pending))} onclick={() => onanswer('always')}>
      {t(alwaysLabel(pending))}
    </button>
    <button class="bt no" onclick={() => onanswer('deny')}>{t('confirm.deny')}</button>
  </div>
  {#if !compact}
    <p class="hint">{t(pending.allowAlways ? 'confirm.hint' : 'confirm.hint_no_always')}</p>
  {/if}
</div>

<style>
  .cf {
    height: 100%;
    box-sizing: border-box;
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    text-align: left;
    overflow: hidden;
  }
  .cf.compact {
    height: auto;
    flex: none;
    margin: 0 14px 10px;
    padding: 14px 16px;
    border-radius: 20px;
    background: var(--glass);
    border: 1px solid rgba(255, 198, 92, 0.3);
    box-shadow: 0 0 60px rgba(255, 198, 92, 0.07);
  }
  .cf.compact.lvl-high {
    border-color: rgba(255, 107, 125, 0.38);
    box-shadow: 0 0 60px rgba(255, 107, 125, 0.1);
  }
  .hd {
    flex: none;
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 11px;
    font-weight: 800;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--warn);
  }
  .lvl-low .hd {
    color: var(--ok);
  }
  .lvl-high .hd {
    color: var(--bad);
  }
  .timer {
    margin-left: auto;
    color: var(--mute);
    letter-spacing: 0.04em;
    font-variant-numeric: tabular-nums;
  }
  h3 {
    flex: none;
    margin: 10px 0 8px;
    font-size: 17px;
    font-weight: 700;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The exact command or path. It scrolls, so the buttons never leave the card.
     A command gets five lines. A target written in lines gets a taller box from
     the script: a screen recording names the microphone and the sound on lines
     of their own, and none of them may hide below a scroll bar. */
  pre {
    flex: 0 1 auto;
    min-height: 36px;
    max-height: 110px;
    box-sizing: border-box;
    margin: 0;
    font: 12.5px/1.45 var(--mono);
    color: var(--ink-v);
    background: rgba(0, 0, 0, 0.5);
    border: 1px solid var(--line);
    border-radius: 12px;
    padding: 8px 12px;
    white-space: pre-wrap;
    word-break: break-all;
    overflow: auto;
    user-select: text;
  }
  /* A scenario by event: its limits and its whole instruction stay in sight (11 lines). */
  pre.whole {
    max-height: 218px;
    word-break: normal;
    overflow-wrap: anywhere;
  }
  .why {
    flex: none;
    margin: 9px 0 13px;
    font-size: 13px;
    color: var(--mute);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .compact .why {
    margin: 8px 0 11px;
  }
  .acts {
    flex: none;
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .hint {
    flex: none;
    margin: 12px 0 0;
    font-size: 12px;
    color: var(--mute);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* English interface, Russian description: said before the description itself. */
  .untr {
    flex: none;
    margin: 8px 0 -4px;
    font-size: 12px;
    color: var(--warn);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
