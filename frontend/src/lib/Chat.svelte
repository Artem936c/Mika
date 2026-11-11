<script lang="ts">
  import { tick } from 'svelte';
  import type { ChatItem } from './chat';
  import type { AssistantState } from './island';
  import { STATE_LABELS } from './island';
  import ConfirmCard from './ConfirmCard.svelte';
  import type { Pending } from './confirm';

  let {
    items,
    status,
    name,
    speak,
    pending,
    needsSetup = false,
    onanswer,
    onsend,
    onstop,
    onclose,
    onsettings,
    onspeak,
    onnew,
  }: {
    items: ChatItem[];
    status: AssistantState;
    name: string;
    speak: boolean;
    pending: Pending | null;
    needsSetup?: boolean;
    onanswer: (a: 'once' | 'always' | 'deny') => void;
    onsend: (text: string) => void;
    onstop: () => void;
    onclose: () => void;
    onsettings: () => void;
    onspeak: (v: boolean) => void;
    onnew: () => void;
  } = $props();

  let draft = $state('');
  let list: HTMLDivElement | undefined = $state();
  let input: HTMLTextAreaElement | undefined = $state();
  const busy = $derived(status !== 'idle');

  $effect(() => {
    void items.length;
    void items[items.length - 1]?.text;
    tick().then(() => list?.scrollTo({ top: list.scrollHeight }));
  });

  $effect(() => {
    input?.focus();
  });

  function submit() {
    const text = draft.trim();
    if (!text) return;
    onsend(text);
    draft = '';
  }

  function onkey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === 'Escape') {
      onclose();
    }
  }
</script>

<div class="chat">
  <header>
    <div class="who">
      <span class="orb"></span>
      <b>{name}</b>
      <span class="status" class:busy>{STATE_LABELS[status]}</span>
    </div>
    <div class="tools">
      <button class="tg" class:on={speak} title="Озвучивать ответы" onclick={() => onspeak(!speak)}>Озвучка</button>
      <button class="tg" disabled title="Голос появится в эпике 5. Сейчас микрофон выключен">Микрофон</button>
      <button class="tg" disabled title="Просмотр экрана выключен">Экран</button>
      <button class="ib" title="Новый разговор: Мика забудет этот чат" aria-label="Новый разговор" onclick={onnew}>＋</button>
      <button class="ib" title="Настройки" aria-label="Настройки" onclick={onsettings}>⚙</button>
      <button class="ib" title="Свернуть (Esc)" aria-label="Свернуть" onclick={onclose}>✕</button>
    </div>
  </header>

  {#if needsSetup}
    <div class="setup">
      <span>Чтобы Мика отвечала, укажи провайдера и ключ.</span>
      <button onclick={onsettings}>Настроить</button>
    </div>
  {/if}

  <div class="msgs" bind:this={list}>
    {#if items.length === 0}
      <p class="empty">Напиши, что сделать. Например: «который час?»</p>
    {/if}
    {#each items as it, i (i)}
      {#if it.kind === 'user'}
        <div class="bub u">{it.text}</div>
      {:else if it.kind === 'assistant'}
        <div class="bub a" class:pending={it.pending}>{it.text}</div>
      {:else if it.kind === 'tool'}
        <div class="step" class:fail={it.ok === false} class:run={it.pending}>
          <span class="dot"></span>
          <span class="tname">{it.text}</span>
          <span class="meta">{it.name}{#if it.ms !== undefined} · {it.ms} мс{/if}</span>
        </div>
      {:else}
        <div class="err">{it.text}</div>
      {/if}
    {/each}
  </div>

  {#if pending}
    <ConfirmCard {pending} {onanswer} compact />
  {/if}

  <div class="comp">
    <textarea
      bind:this={input}
      bind:value={draft}
      onkeydown={onkey}
      rows="1"
      placeholder="Сообщение для {name}…"
      aria-label="Сообщение"
    ></textarea>
    {#if busy && !draft.trim()}
      <button class="send stop" onclick={onstop} title="Остановить" aria-label="Остановить">■</button>
    {:else}
      <button class="send" onclick={submit} disabled={!draft.trim()} title="Отправить (Enter)" aria-label="Отправить">↑</button>
    {/if}
  </div>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 100%;
    animation: fade 0.35s var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
      filter: blur(8px);
    }
  }
  header {
    height: 54px;
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 12px 0 16px;
    border-bottom: 1px solid var(--line);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 14px;
  }
  .orb {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: radial-gradient(circle at 35% 30%, #fff8, transparent 40%), linear-gradient(135deg, var(--v), var(--p));
  }
  .status {
    font-size: 12px;
    color: var(--mute);
  }
  .status.busy {
    color: var(--c);
  }
  .tools {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .tg {
    height: 28px;
    padding: 0 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 600;
    color: var(--mute);
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--line);
    cursor: pointer;
  }
  .tg.on {
    color: var(--ink);
    border-color: rgba(143, 125, 255, 0.5);
    background: rgba(143, 125, 255, 0.16);
  }
  .tg:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .ib {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgba(255, 255, 255, 0.07);
    border: 1px solid var(--line);
    color: var(--mute);
    cursor: pointer;
  }
  .ib:hover {
    color: var(--ink);
  }
  .setup {
    margin: 10px 12px 0;
    padding: 10px 12px;
    border-radius: 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    font-size: 12.5px;
    background: rgba(143, 125, 255, 0.12);
    border: 1px solid rgba(143, 125, 255, 0.35);
  }
  .setup button {
    border: 0;
    border-radius: 10px;
    padding: 6px 12px;
    font-weight: 700;
    background: linear-gradient(135deg, var(--v), #6b58e6);
    cursor: pointer;
  }
  .msgs {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
    user-select: text;
  }
  .empty {
    margin: auto;
    color: var(--dim);
  }
  .bub {
    max-width: 82%;
    padding: 9px 13px;
    border-radius: 16px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .bub.u {
    align-self: flex-end;
    background: linear-gradient(135deg, var(--v), #6b58e6);
    border-bottom-right-radius: 6px;
  }
  .bub.a {
    align-self: flex-start;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid var(--line);
    border-bottom-left-radius: 6px;
  }
  .bub.a.pending::after {
    content: '▍';
    color: var(--v);
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
  .step {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--mute);
    font-weight: 600;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
    flex: none;
  }
  .step.run .dot {
    background: var(--c);
    animation: blink 0.8s steps(2) infinite;
  }
  .step.fail .dot {
    background: var(--bad);
  }
  .meta {
    color: var(--dim);
    font-weight: 500;
  }
  .err {
    align-self: stretch;
    padding: 8px 12px;
    border-radius: 12px;
    color: var(--bad);
    background: rgba(255, 107, 125, 0.08);
    border: 1px solid rgba(255, 107, 125, 0.25);
  }
  .comp {
    margin: 0 12px 12px;
    min-height: 46px;
    border-radius: 23px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px 4px 16px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid var(--line2);
  }
  textarea {
    flex: 1;
    resize: none;
    border: 0;
    outline: 0;
    background: transparent;
    max-height: 90px;
    font-size: 14px;
  }
  .send {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 0;
    background: linear-gradient(135deg, var(--v), #6b58e6);
    font-size: 16px;
    font-weight: 800;
    cursor: pointer;
  }
  .send:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .send.stop {
    background: rgba(255, 107, 125, 0.2);
    color: var(--bad);
    font-size: 12px;
  }
</style>
