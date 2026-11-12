<script lang="ts">
  // First run: the provider, then what she may use. Everything that listens,
  // looks or goes online stays off until the user turns it on here or later.
  import { invoke } from '@tauri-apps/api/core';
  import Avatar from './Avatar.svelte';
  import Switch from './Switch.svelte';
  import { dragRegion } from './drag';
  import { t } from './i18n';
  import Providers from './settings/Providers.svelte';

  let { name, ondone }: { name: string; ondone: () => void } = $props();

  let step = $state(0);
  let web = $state(false);
  let screen = $state(false);
  let mic = $state(false);
  let wake = $state(false);
  let heard = $state('');
  let note = $state('');
  let listening = $state(false);

  async function setWeb(v: boolean) {
    web = v;
    await invoke('set_web_enabled', { value: v }).catch((e) => (note = String(e)));
  }

  async function setScreen(v: boolean) {
    screen = v;
    await invoke('set_screen_enabled', { value: v }).catch((e) => (note = String(e)));
  }

  async function setMic(v: boolean) {
    mic = v;
    if (!v) wake = false;
    await invoke('set_microphone_enabled', { value: v }).catch((e) => (note = String(e)));
  }

  async function setWake(v: boolean) {
    wake = v;
    await invoke('set_wake', { enabled: v, phrase: name }).catch((e) => {
      wake = false;
      note = String(e);
    });
  }

  async function testMic() {
    note = '';
    heard = '';
    listening = true;
    try {
      heard = await invoke<string>('test_microphone');
    } catch (e) {
      if (String(e)) note = String(e);
    } finally {
      listening = false;
    }
  }

  async function finish() {
    await invoke('finish_onboarding').catch(() => {});
    ondone();
  }
</script>

<div class="wel">
  <aside class="side" use:dragRegion>
    <div class="who"><Avatar view="head" emotion="joy" size={56} /></div>
    <b>{t('welcome.hello', { name })}</b>
    <p>{t('welcome.lead')}</p>
    <ol>
      <li class:on={step === 0}>{t('welcome.step_provider')}</li>
      <li class:on={step === 1}>{t('welcome.step_privacy')}</li>
    </ol>
  </aside>

  <section class="page">
    {#if step === 0}
      <h3>{t('welcome.step_provider')}</h3>
      <p class="lead">{t('welcome.provider_lead')}</p>
      <Providers />
    {:else}
      <h3>{t('welcome.step_privacy')}</h3>
      <p class="lead">{t('welcome.privacy_lead')}</p>
      <div class="stack">
        <Switch checked={web} title={t('tools.web')} sub={t('tools.web_sub')} onchange={setWeb} />
        <Switch checked={screen} title={t('tools.screen')} sub={t('tools.screen_sub')} onchange={setScreen} />
        <Switch checked={mic} title={t('voice.mic')} sub={t('welcome.mic_sub')} onchange={setMic} />
        {#if mic}
          <Switch checked={wake} title={t('voice.wake', { name })} sub={t('voice.wake_sub')} onchange={setWake} />
          <div class="row">
            <button class="bt" onclick={testMic} disabled={listening}>{t(listening ? 'welcome.say' : 'voice.mic_test')}</button>
            {#if heard}<span class="ok">{t('voice.heard', { text: heard })}</span>{/if}
          </div>
        {/if}
      </div>
    {/if}
    {#if note}<p class="err">{note}</p>{/if}
    <div class="nav">
      {#if step > 0}<button class="bt" onclick={() => (step -= 1)}>{t('welcome.back')}</button>{/if}
      <span class="grow"></span>
      <button class="link" onclick={finish}>{t('welcome.skip')}</button>
      {#if step === 0}
        <button class="bt pri" onclick={() => (step = 1)}>{t('welcome.next')}</button>
      {:else}
        <button class="bt pri" onclick={finish}>{t('welcome.done')}</button>
      {/if}
    </div>
  </section>
</div>

<style>
  .wel {
    display: grid;
    grid-template-columns: 240px 1fr;
    width: 100%;
    height: 100%;
    text-align: left;
    background: #0d0e17;
  }
  .side {
    padding: 22px 20px;
    border-right: 1px solid var(--line);
    background: linear-gradient(180deg, #151331, #0b0b16);
  }
  .who {
    margin-bottom: 14px;
  }
  .side b {
    display: block;
    font-size: 18px;
    font-weight: 800;
    margin-bottom: 6px;
  }
  .side p {
    margin: 0 0 18px;
    font-size: 13px;
    color: var(--mute);
    line-height: 1.5;
  }
  ol {
    margin: 0;
    padding-left: 18px;
    font-size: 13px;
    color: var(--dim);
    display: grid;
    gap: 8px;
  }
  li.on {
    color: #fff;
    font-weight: 700;
  }
  .page {
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    padding: 22px 26px 18px;
    display: flex;
    flex-direction: column;
  }
  h3 {
    margin: 0 0 4px;
    font-size: 19px;
    font-weight: 800;
  }
  .lead {
    margin: 0 0 16px;
    color: var(--mute);
    font-size: 13.5px;
  }
  .stack {
    display: grid;
    gap: 10px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .ok {
    font-size: 13px;
    color: var(--ok);
    user-select: text;
  }
  .err {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--bad);
    user-select: text;
  }
  .nav {
    margin-top: auto;
    padding-top: 16px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .grow {
    flex: 1;
  }
  .link {
    all: unset;
    cursor: pointer;
    font-size: 13px;
    color: var(--mute);
  }
  .link:hover {
    color: #fff;
  }
</style>
