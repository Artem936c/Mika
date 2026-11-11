<script lang="ts">
  // Content of the four small island forms: idle, listening, thinking, speaking.
  import { onMount } from 'svelte';
  import Avatar from './Avatar.svelte';
  import Bars from './Bars.svelte';
  import Icon from './Icon.svelte';
  import type { Emotion } from './avatar';
  import { speakWords, toolLabel } from './chat';
  import { t } from './i18n';
  import { stateLabel, type AssistantState } from './island';

  let {
    status,
    name,
    reply,
    work,
    tool,
    emotion,
    speak,
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
    onspeak: (v: boolean) => void;
  } = $props();

  let clock = $state('');
  onMount(() => {
    const tick = () => (clock = new Date().toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' }));
    tick();
    const id = setInterval(tick, 10_000);
    return () => clearInterval(id);
  });

  // Two lines of 15 px text fit about 110 characters in the 620 px form.
  const words = $derived(speakWords(reply, 110));
</script>

{#if status === 'idle'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={30} />
    <span class="name">{name}</span>
    <span class="grow"></span>
    <span class="mute">{clock}</span>
    <span class="pad"></span>
  </div>
{:else if status === 'listening'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={36} />
    <Bars count={26} />
    <span class="end">{t(stateLabel('listening'))}</span>
  </div>
{:else if status === 'thinking'}
  <div class="rowl">
    <Avatar view="head" {emotion} size={36} />
    <span class="shim">{work || t('face.thinking')}</span>
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
