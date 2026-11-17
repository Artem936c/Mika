<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import Events from './Events.svelte';
  import Switch from '../Switch.svelte';
  import { dateLocale, isMessageKey, t } from '../i18n';

  interface Command {
    id: number;
    name: string;
    prompt: string;
  }
  /** A skill she saved (decision 0047): steps of tools with their arguments. */
  interface Skill {
    id: number;
    name: string;
    description: string;
    params: { name: string; about: string }[];
    steps: { tool: string; args: Record<string, unknown> }[];
  }
  interface Skills {
    skills: Skill[];
    suggest: boolean;
  }
  interface Reminder {
    id: number;
    due_at: number;
    text: string;
    kind: 'remind' | 'task';
    repeat: string | { every: number };
  }
  interface McpServer {
    name: string;
    command: string;
    args: string[];
    url: string;
    has_token: boolean;
    enabled: boolean;
    tools: number | null;
    error: string | null;
  }
  interface Telegram {
    enabled: boolean;
    user_id: number;
    has_token: boolean;
    status: string;
    voice_in: boolean;
    voice_reply: boolean;
    stt_ready: boolean;
    tts_ready: boolean;
    ffmpeg_found: boolean;
    voice_max_s: number;
  }

  let commands = $state<Command[]>([]);
  let skills = $state<Skills>({ skills: [], suggest: true });
  let reminders = $state<Reminder[]>([]);
  let servers = $state<McpServer[]>([]);
  let tg = $state<Telegram | null>(null);

  let cmdName = $state('');
  let cmdPrompt = $state('');
  let mcpName = $state('');
  let mcpLine = $state('');
  let mcpToken = $state('');
  let mcpBusy = $state(false);
  let tgToken = $state('');
  let tgUser = $state('');
  let tgNote = $state('');
  let error = $state('');

  onMount(load);

  async function load() {
    try {
      [commands, skills, reminders, servers, tg] = await Promise.all([
        invoke<Command[]>('list_commands'),
        invoke<Skills>('list_skills'),
        invoke<Reminder[]>('list_reminders'),
        invoke<McpServer[]>('list_mcp'),
        invoke<Telegram>('get_telegram'),
      ]);
      tgUser = tg.user_id ? String(tg.user_id) : '';
    } catch (e) {
      error = String(e);
    }
  }

  async function run(command: string, args: Record<string, unknown>): Promise<boolean> {
    try {
      await invoke(command, args);
      error = '';
      await load();
      return true;
    } catch (e) {
      error = String(e);
      return false;
    }
  }

  async function addCommand() {
    if (await run('save_command', { name: cmdName, prompt: cmdPrompt })) {
      cmdName = '';
      cmdPrompt = '';
    }
  }

  async function addServer() {
    mcpBusy = true;
    if (await run('save_mcp_server', { name: mcpName, commandLine: mcpLine, enabled: true, token: mcpToken ? mcpToken : null })) {
      mcpName = '';
      mcpLine = '';
      mcpToken = '';
    }
    mcpBusy = false;
  }

  async function toggleServer(s: McpServer, enabled: boolean) {
    mcpBusy = true;
    const line = s.url || [s.command, ...s.args].map((a) => (/\s/.test(a) || a === '' ? `"${a}"` : a)).join(' ');
    await run('save_mcp_server', { name: s.name, commandLine: line, enabled, token: null });
    mcpBusy = false;
  }

  async function removeServer(name: string) {
    mcpBusy = true;
    await run('remove_mcp_server', { name });
    mcpBusy = false;
  }

  async function saveTelegram(enabled: boolean) {
    tgNote = '';
    const userId = Number(tgUser.trim() || 0);
    if (!Number.isSafeInteger(userId) || userId < 0) {
      error = t('ext.tg_user_bad');
      return;
    }
    if (await run('save_telegram', { enabled, userId, token: tgToken ? tgToken : null })) tgToken = '';
  }

  // Both voice switches go together; the bridge reads them at every message.
  async function setTgVoice(voiceIn: boolean, voiceReply: boolean) {
    try {
      tg = await invoke<Telegram>('set_telegram_voice', { voiceIn, voiceReply });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  async function checkTelegram() {
    if (!tg) return;
    await saveTelegram(tg.enabled);
    try {
      const name = await invoke<string>('test_telegram');
      tgNote = t('ext.tg_bot', { name });
      error = '';
    } catch (e) {
      error = String(e);
    }
  }

  /** One step for the list: the tool and its arguments in short. */
  function stepLine(step: Skill['steps'][number], n: number): string {
    const args = Object.keys(step.args ?? {}).length ? ' ' + JSON.stringify(step.args) : '';
    return `${n}. ${step.tool}${args}`;
  }

  function when(dueAt: number): string {
    return new Date(dueAt * 1000).toLocaleString(dateLocale(), { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' });
  }

  function repeatLabel(r: Reminder['repeat']): string {
    if (typeof r === 'object') return t('ext.repeat_every', { n: r.every });
    const key = `ext.repeat_${r}`;
    return r !== 'none' && isMessageKey(key) ? t(key) : '';
  }

  const tgStatus = $derived.by(() => {
    const key = `ext.tg_status.${tg?.status ?? 'off'}`;
    return isMessageKey(key) ? t(key) : '';
  });
</script>

<div class="lbl">{t('ext.commands')}</div>
<p class="hint">{t('ext.commands_sub')}</p>
{#each commands as c (c.id)}
  <div class="item">
    <div class="txt"><b>{c.name}</b><small>{c.prompt}</small></div>
    <button class="ib" title={t('ext.remove')} aria-label={t('ext.remove')} onclick={() => run('delete_command', { name: c.name })}><Icon name="x" size={14} /></button>
  </div>
{/each}
<div class="form">
  <input class="fld" bind:value={cmdName} maxlength="60" placeholder={t('ext.command_name')} />
  <textarea class="fld" rows="2" bind:value={cmdPrompt} maxlength="2000" placeholder={t('ext.command_prompt')}></textarea>
  <button class="bt" onclick={addCommand} disabled={!cmdName.trim() || !cmdPrompt.trim()}>{t('ext.command_add')}</button>
</div>

<div class="lbl">{t('ext.skills')}</div>
<p class="hint">{t('ext.skills_sub')}</p>
{#if skills.skills.length === 0}
  <p class="hint">{t('ext.skills_none')}</p>
{/if}
{#each skills.skills as s (s.id)}
  <div class="item skill">
    <div class="txt">
      <b>{s.name}</b>
      {#if s.description}<small>{s.description}</small>{/if}
      {#if s.params.length}<small>{t('ext.skill_params', { list: s.params.map((p) => (p.about ? `${p.name} (${p.about})` : p.name)).join(', ') })}</small>{/if}
      <ol class="steps">
        {#each s.steps as step, i (i)}
          <li title={stepLine(step, i + 1)}>{stepLine(step, i + 1)}</li>
        {/each}
      </ol>
    </div>
    <button class="ib" title={t('ext.remove')} aria-label={t('ext.remove')} onclick={() => run('delete_skill', { name: s.name })}><Icon name="x" size={14} /></button>
  </div>
{/each}
<Switch checked={skills.suggest} title={t('ext.skills_suggest')} sub={t('ext.skills_suggest_sub')} onchange={(v) => run('set_skill_offers', { on: v })} />

<div class="lbl">{t('ext.schedule')}</div>
{#if reminders.length === 0}
  <p class="hint">{t('ext.schedule_none')}</p>
{/if}
{#each reminders as r (r.id)}
  <div class="item">
    <div class="txt">
      <b>{when(r.due_at)}{#if r.kind === 'task'}<i class="tag">{t('ext.schedule_task')}</i>{/if}{#if repeatLabel(r.repeat)}<i class="tag dim">{repeatLabel(r.repeat)}</i>{/if}</b>
      <small>{r.text}</small>
    </div>
    <button class="ib" title={t('ext.schedule_cancel')} aria-label={t('ext.schedule_cancel')} onclick={() => run('cancel_reminder', { id: r.id })}><Icon name="x" size={14} /></button>
  </div>
{/each}
<Events />

<div class="lbl">{t('ext.mcp')}</div>
<p class="hint">{t('ext.mcp_sub')}</p>
{#each servers as s (s.name)}
  <div class="item">
    <input type="checkbox" checked={s.enabled} disabled={mcpBusy} aria-label={s.name} onchange={(e) => toggleServer(s, e.currentTarget.checked)} />
    <div class="txt">
      <b>
        {s.name}
        {#if !s.enabled}<i class="tag dim">{t('ext.mcp_off')}</i>
        {:else if s.error}<i class="tag bad">{t('ext.mcp_failed')}</i>
        {:else if s.tools !== null}<i class="tag ok">{t('ext.mcp_tools', { count: s.tools })}</i>
        {:else}<i class="tag dim">{t('ext.mcp_starting')}</i>{/if}
      </b>
      <small title={s.error ?? ''}>{s.error ?? (s.url || [s.command, ...s.args].join(' '))}</small>
    </div>
    <button class="ib" disabled={mcpBusy} title={t('ext.remove')} aria-label={t('ext.remove')} onclick={() => removeServer(s.name)}><Icon name="x" size={14} /></button>
  </div>
{/each}
<div class="form">
  <input class="fld" bind:value={mcpName} maxlength="40" placeholder={t('ext.mcp_name')} spellcheck="false" />
  <input class="fld" bind:value={mcpLine} placeholder={t('ext.mcp_command')} spellcheck="false" />
  {#if mcpLine.trim().startsWith('http')}
    <input class="fld" type="password" bind:value={mcpToken} placeholder={t('ext.mcp_token')} autocomplete="off" />
  {/if}
  <button class="bt" onclick={addServer} disabled={mcpBusy || !mcpName.trim() || !mcpLine.trim()}>{t(mcpBusy ? 'ext.mcp_starting' : 'ext.mcp_add')}</button>
</div>

<div class="lbl">{t('ext.tg')}{#if tgStatus}<span class="badge" class:on={tg?.status === 'on'}>{tgStatus}</span>{/if}</div>
{#if tg}
  <Switch checked={tg.enabled} title={t('ext.tg_enable')} sub={t('ext.tg_sub')} onchange={saveTelegram} />
  <div class="form">
    <input class="fld" type="password" bind:value={tgToken} placeholder={tg.has_token ? t('ext.tg_token_saved') : t('ext.tg_token')} autocomplete="off" />
    <input class="fld" bind:value={tgUser} inputmode="numeric" placeholder={t('ext.tg_user')} />
    <div class="row">
      <button class="bt" onclick={() => saveTelegram(tg!.enabled)}>{t('providers.save')}</button>
      <button class="bt" onclick={checkTelegram}>{t('ext.tg_check')}</button>
      {#if tgNote}<span class="ok">{tgNote}</span>{/if}
    </div>
  </div>
  <div class="voice">
    <Switch checked={tg.voice_in} title={t('ext.tg_voice_in')} sub={t('ext.tg_voice_in_sub', { minutes: Math.round(tg.voice_max_s / 60) })} onchange={(v) => setTgVoice(v, tg!.voice_reply)} />
    {#if tg.voice_in}
      {#if !tg.stt_ready}<p class="warn">{t('ext.tg_voice_no_stt')}</p>{/if}
      <Switch checked={tg.voice_reply} title={t('ext.tg_voice_reply')} sub={t('ext.tg_voice_reply_sub')} onchange={(v) => setTgVoice(true, v)} />
      {#if tg.voice_reply && !tg.tts_ready}
        <p class="warn">{t('ext.tg_voice_no_tts')}</p>
      {:else if tg.voice_reply && !tg.ffmpeg_found}
        <p class="hint">{t('ext.tg_voice_no_ffmpeg')}</p>
      {/if}
    {/if}
  </div>
{/if}
{#if error}<p class="err">{error}</p>{/if}

<style>
  .hint {
    margin: 0 2px 10px;
    font-size: 12.5px;
    color: var(--dim);
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 8px 9px 14px;
    margin-bottom: 6px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
  }
  .item input[type='checkbox'] {
    accent-color: var(--v);
  }
  .txt {
    flex: 1;
    min-width: 0;
  }
  .txt b {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
  }
  .txt small {
    display: block;
    color: var(--mute);
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .item .ib {
    width: 26px;
    height: 26px;
  }
  .item.skill {
    align-items: flex-start;
  }
  .steps {
    margin: 4px 0 0;
    padding-left: 0;
    list-style: none;
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--mute);
  }
  .steps li {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .tag {
    font-style: normal;
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 999px;
    color: var(--c);
    background: rgba(111, 227, 255, 0.1);
  }
  .tag.dim {
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
  }
  .tag.ok {
    color: var(--ok);
    background: rgba(95, 227, 161, 0.1);
  }
  .tag.bad {
    color: var(--bad);
    background: rgba(255, 107, 125, 0.12);
  }
  .form {
    display: grid;
    gap: 8px;
    margin-top: 8px;
    justify-items: start;
  }
  .lbl {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 22px;
  }
  .lbl:first-child {
    margin-top: 0;
  }
  .badge {
    font-size: 10.5px;
    font-weight: 800;
    letter-spacing: 0.06em;
    padding: 2px 8px;
    border-radius: 999px;
    color: var(--mute);
    background: rgba(255, 255, 255, 0.06);
  }
  .badge.on {
    color: var(--ok);
    background: rgba(95, 227, 161, 0.1);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ok {
    font-size: 12.5px;
    color: var(--ok);
  }
  /* The two voice switches of Telegram and the notes under them. */
  .voice {
    display: grid;
    gap: 8px;
    margin-top: 8px;
  }
  .voice .hint {
    margin: 0 2px;
  }
  .warn {
    margin: 0 2px;
    font-size: 12.5px;
    color: var(--warn);
  }
  .err {
    margin: 12px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
</style>
