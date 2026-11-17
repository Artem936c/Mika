<script lang="ts">
  // Content of the four small island forms: idle, listening, thinking, speaking.
  import { onMount } from 'svelte';
  import Avatar from './Avatar.svelte';
  import Bars from './Bars.svelte';
  import Icon from './Icon.svelte';
  import type { Emotion } from './avatar';
  import { countdown, speakWords, toolLabel } from './chat';
  import type { DictationPhase } from './hotkeys';
  import { t } from './i18n';
  import { stateLabel, type AssistantState } from './island';
  import { dateLocale } from './locale.svelte';

  let {
    status,
    name,
    reply,
    work,
    tool,
    emotion,
    speak,
    weather = null,
    timer = null,
    track = null,
    dictation = null,
    paused = false,
    onspeak,
  }: {
    status: AssistantState;
    name: string;
    /** Reply text so far. */
    reply: string;
    /** What the running tool does, in plain words. */
    work: string;
    /** Name of the running tool, empty when none. */
    tool: string;
    emotion: Emotion;
    speak: boolean;
    /** Temperature outside, whole degrees. */
    weather?: { temp: number; text: string } | null;
    /** The nearest reminder: unix seconds and its text. */
    timer?: { due_at: number; text: string } | null;
    /** What plays now in the user's player. */
    track?: { title: string; artist: string } | null;
    /** Where a dictation into another program is; null when none runs. */
    dictation?: DictationPhase | null;
    /** The pause after a full stop: she waits to be called (decision 0045). */
    paused?: boolean;
    onspeak: (v: boolean) => void;
  } = $props();

  let clock = $state('');
  let now = $state(Date.now());
  onMount(() => {
    const tick = () => {
      now = Date.now();
      clock = new Date(now).toLocaleTimeString(dateLocale(), { hour: '2-digit', minute: '2-digit' });
    };
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  });

  // A reminder within the next hour replaces the clock with a countdown.
  const left = $derived(timer && timer.due_at * 1000 - now < 3_600_000 ? countdown(timer.due_at, now) : '');
  const degrees = $derived(weather ? `${weather.temp > 0 ? '+' : ''}${weather.temp}°` : '');

  // Two lines of 15 px text fit about 110 characters in the 620 px form.
  const words = $derived(speakWords(reply, 110));
</script>

{#if status === 'idle' && paused}
  <div class="rowl">
    <Avatar view="head" {emotion} size={30} />
    <span class="name paused">{t('face.paused')}</span>
    <span class="pad"></span>
  </div>
{:else if status === 'idle'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={30} />
    {#if track}
      <span class="name song" title={track.artist ? `${track.title} — ${track.artist}` : track.title}>
        <Icon name="note" size={12} />{track.title}
      </span>
    {:else}
      <span class="name">{name}</span>
    {/if}
    <span class="grow"></span>
    {#if degrees}<span class="mute" title={weather?.text}>{degrees}</span>{/if}
    {#if left}
      <span class="timer" title={t('face.timer_title', { text: timer?.text ?? '' })}><Icon name="bell" size={12} />{left}</span>
    {:else}
      <span class="mute">{clock}</span>
    {/if}
    <span class="pad"></span>
  </div>
{:else if status === 'listening'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={36} />
    <Bars count={26} />
    <span class="end">{t(dictation ? 'dictation.island' : stateLabel('listening'))}</span>
  </div>
{:else if status === 'thinking'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={36} />
    <span class="shim">{work || t(dictation === 'typing' ? 'dictation.typing' : dictation ? 'dictation.recognising' : 'face.thinking')}</span>
    <span class="grow"></span>
    {#if tool}
      <span class="tool"><Icon name="wrench" size={12} />{t(toolLabel(tool))}</span>
    {/if}
    <span class="orb"></span>
    <span class="pad"></span>
  </div>
{:else if status === 'speaking'}
  <div class="rowl speak">
    <Avatar view="head" {emotion} size={50} talking />
    <p class="say">
      {#each words as w (w.i)}<span>{`${w.word} `}</span>{:else}{t(stateLabel('speaking'))}{/each}
    </p>
    <button
      class="ib"
      class:on={speak}
      title={t('chat.speak_title')}
      aria-label={t('chat.speak_title')}
      aria-pressed={speak}
      onclick={() => onspeak(!speak)}
    >
      <Icon name={speak ? 'vol' : 'mute'} />
    </button>
  </div>
{/if}

<style>
  .rowl {
    width: 100%;
    height: 100%;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 8px;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
  }
  .speak {
    gap: 14px;
    padding: 0 14px;
  }
  .grow {
    flex: 1;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* Calm, not an alarm: the muted colour of the clock. */
  .paused {
    flex: 1;
    color: var(--mute);
    font-weight: 500;
  }
  .pad {
    width: 4px;
  }
  .end {
    padding-right: 8px;
  }
  .mute {
    color: var(--mute);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .song {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--p);
  }
  .timer {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--c);
    font-variant-numeric: tabular-nums;
  }
  .shim {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    background: linear-gradient(90deg, #8d93ab 20%, #fff 50%, #8d93ab 80%);
    background-size: 200% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: shim 1.6s linear infinite;
  }
  @keyframes shim {
    to {
      background-position: -200% 0;
    }
  }
  .tool {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-weight: 600;
    color: var(--c);
    background: rgba(111, 227, 255, 0.1);
    border: 1px solid rgba(111, 227, 255, 0.25);
    padding: 3px 9px;
    border-radius: 999px;
  }
  .orb {
    flex: none;
    width: 16px;
    height: 16px;
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
  .say {
    flex: 1;
    min-width: 0;
    margin: 0;
    max-height: 2.7em;
    overflow: hidden;
    font-size: 15px;
    font-weight: 500;
    line-height: 1.35;
    white-space: normal;
  }
  .say span {
    animation: word 0.35s var(--ease) backwards;
  }
  @keyframes word {
    from {
      opacity: 0;
      translate: 0 5px;
      filter: blur(4px);
    }
  }
  .ib {
    width: 36px;
    height: 36px;
  }
  @media (prefers-reduced-motion: reduce) {
    .shim,
    .orb,
    .say span {
      animation: none;
    }
    .shim {
      color: var(--ink);
    }
  }
</style>
