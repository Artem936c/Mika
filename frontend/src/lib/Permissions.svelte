<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { isMessageKey, t } from './i18n';
  import { ruleLabel } from './rules';

  let { name }: { name: string } = $props();

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

  // "Ask" removes the rule: without a rule the assistant asks every time.
  async function ask(id: number) {
    try {
      await invoke('remove_rule', { id });
    } catch (e) {
      error = String(e);
    }
    await load();
  }

  const time = (s: string) => s.replace('T', ' ').slice(0, 16);
  /** Text for a code from the backend, or the code itself when it is new. */
  const label = (prefix: string, code: string) => {
    const key = `${prefix}.${code}`;
    return isMessageKey(key) ? t(key) : code;
  };

  onMount(load);
</script>

{#if rules.length === 0}
  <p class="empty">{t('permissions.none', { name })}</p>
{:else}
  {#each rules as r (r.id)}
    <div class="perm">
      <span class="tool">{r.tool}</span>
      {#if ruleLabel(r.pattern)}
        <span class="named" title={r.pattern}>{t(ruleLabel(r.pattern)!)}</span>
      {:else}
        <code title={r.pattern}>{r.pattern}</code>
      {/if}
      <div class="seg">
        {#if r.decision === 'allow'}
          <button class="on">{t('permissions.always')}</button>
          <button onclick={() => ask(r.id)}>{t('permissions.ask')}</button>
        {:else}
          <button onclick={() => ask(r.id)}>{t('permissions.ask')}</button>
          <button class="on">{t('permissions.deny')}</button>
        {/if}
      </div>
    </div>
  {/each}
{/if}

<div class="lbl">{t('permissions.log')}</div>
{#if audit.length === 0}
  <p class="empty">{t('permissions.log_empty')}</p>
{:else}
  <ul>
    {#each audit as a, i (i)}
      <li>
        <span class="at">{time(a.at)}</span>
        <span class="rk {a.risk}">{label('risk', a.risk)}</span>
        <span class="tool">{a.tool}</span>
        <code title={a.target}>{a.target}</code>
        <span class="d {a.decision}">{label('decision', a.decision)}</span>
        <span class="by">{label('by', a.decided_by)}</span>
      </li>
    {/each}
  </ul>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .empty {
    margin: 0;
    color: var(--mute);
    font-size: 13px;
  }
  .perm {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
    font-size: 14px;
  }
  .perm:first-child {
    border-top: 0;
    padding-top: 0;
  }
  .perm .seg {
    margin-left: auto;
    flex: none;
    padding: 3px;
  }
  .perm .seg button {
    padding: 5px 10px;
    font-size: 12px;
  }
  .perm .seg button.on {
    cursor: default;
  }
  .tool {
    flex: none;
    font-weight: 700;
    font-size: 13px;
  }
  .named {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }
  code {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 12px var(--mono);
    color: #cdd3ff;
    user-select: text;
  }
  .lbl {
    margin-top: 22px;
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
    gap: 10px;
    padding: 8px 12px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    font-size: 12px;
  }
  .at {
    flex: none;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
  }
  .d {
    flex: none;
    font-weight: 700;
    color: var(--ok);
  }
  .d.deny,
  .d.timeout {
    color: var(--bad);
  }
  .by {
    flex: none;
    color: var(--dim);
  }
  .err {
    margin: 10px 0 0;
    font-size: 12.5px;
    color: var(--bad);
  }
</style>
