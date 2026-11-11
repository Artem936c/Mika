<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

  interface Rule {
    id: number;
    tool: string;
    pattern: string;
    decision: 'allow' | 'deny';
    created_at: string;
  }
  interface AuditEntry {
    at: string;
    tool: string;
    target: string;
    risk: string;
    decision: string;
    decided_by: string;
  }

  const DECISIONS: Record<string, string> = {
    allow: 'разрешено правилом',
    allow_once: 'разрешено один раз',
    allow_always: 'разрешено навсегда',
    deny: 'отклонено',
    timeout: 'нет ответа',
  };
  const BY: Record<string, string> = {
    rule: 'правило',
    button: 'кнопка',
    chat: 'чат',
    timeout: 'таймер',
    policy: 'остановка',
  };

  let rules = $state<Rule[]>([]);
  let audit = $state<AuditEntry[]>([]);
  let error = $state('');

  async function load() {
    try {
      rules = await invoke<Rule[]>('list_rules');
      audit = await invoke<AuditEntry[]>('recent_audit', { limit: 30 });
    } catch (e) {
      error = String(e);
    }
  }

  async function revoke(id: number) {
    await invoke('remove_rule', { id });
    await load();
  }

  const time = (s: string) => s.replace('T', ' ').slice(0, 16);

  onMount(load);
</script>

<div class="perm">
  <h3>Постоянные правила</h3>
  {#if rules.length === 0}
    <p class="empty">Правил нет: Мика спрашивает перед каждым действием.</p>
  {:else}
    <ul>
      {#each rules as r (r.id)}
        <li>
          <span class="dec {r.decision}">{r.decision === 'allow' ? 'Разрешено' : 'Запрещено'}</span>
          <span class="tool">{r.tool}</span>
          <code>{r.pattern}</code>
          <button class="bt" onclick={() => revoke(r.id)}>Отозвать</button>
        </li>
      {/each}
    </ul>
  {/if}

  <h3>Журнал действий</h3>
  {#if audit.length === 0}
    <p class="empty">Пока пусто.</p>
  {:else}
    <ul class="log">
      {#each audit as a, i (i)}
        <li>
          <span class="at">{time(a.at)}</span>
          <span class="tool">{a.tool}</span>
          <code title={a.target}>{a.target}</code>
          <span class="d {a.decision}">{DECISIONS[a.decision] ?? a.decision}</span>
          <span class="by">{BY[a.decided_by] ?? a.decided_by}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}
</div>

<style>
  .perm {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h3 {
    margin: 6px 0 0;
    font-size: 12px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--mute);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--line);
    font-size: 12.5px;
    min-width: 0;
  }
  code {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 12px 'Cascadia Code', Consolas, monospace;
    color: #cdd3ff;
  }
  .tool {
    font-weight: 700;
  }
  .dec.allow,
  .d.allow,
  .d.allow_once,
  .d.allow_always {
    color: var(--ok);
  }
  .dec.deny,
  .d.deny,
  .d.timeout {
    color: var(--bad);
  }
  .at,
  .by {
    color: var(--dim);
    white-space: nowrap;
  }
  .bt {
    height: 26px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--line2);
    background: rgba(255, 255, 255, 0.05);
    color: var(--bad);
    font-size: 12px;
    font-weight: 700;
    cursor: pointer;
  }
  .empty {
    margin: 0;
    color: var(--dim);
    font-size: 12.5px;
  }
  .err {
    color: var(--bad);
  }
</style>
