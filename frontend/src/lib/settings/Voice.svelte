<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Switch from '../Switch.svelte';
  import { t } from '../i18n';

  type Engine = 'windows' | 'provider';
  interface VoiceSettings {
    speak_replies: boolean;
    voice: string;
    rate: number;
    voices: { name: string; language: string }[];
    has_russian: boolean;
    tts: Engine;
    tts_model: string;
    tts_voice: string;
    stt_model: string;
    stt_language: string;
    speech_base_url: string;
    has_speech_key: boolean;
    llm_base_url: string;
    microphone_enabled: boolean;
    microphone_found: boolean;
    listen_shortcut: string;
    input_device: string;
    input_devices: string[];
    wake_enabled: boolean;
    wake_phrase: string;
    wake_status: string;
  }

  let { name }: { name: string } = $props();

  let s = $state<VoiceSettings | null>(null);
  let key = $state('');
  let note = $state('');
  let noteKind = $state<'ok' | 'bad' | ''>('');
  let heard = $state('');
  let listening = $state(false);

  onMount(async () => {
    try {
      s = await invoke<VoiceSettings>('get_voice_settings');
    } catch (e) {
      fail(e);
    }
  });

  function fail(e: unknown) {
    note = String(e);
    noteKind = 'bad';
  }

  async function setSpeak(v: boolean) {
    if (!s) return;
    try {
      await invoke('set_speak_replies', { value: v });
      s.speak_replies = v;
    } catch (e) {
      fail(e);
    }
  }

  async function setMic(v: boolean) {
    if (!s) return;
    try {
      await invoke('set_microphone_enabled', { value: v });
      s.microphone_enabled = v;
    } catch (e) {
      fail(e);
    }
  }

  async function setWake(enabled: boolean) {
    if (!s) return;
    try {
      s.wake_status = await invoke<string>('set_wake', { enabled, phrase: s.wake_phrase });
      s.wake_enabled = enabled;
    } catch (e) {
      fail(e);
    }
  }

  const wakeNote = $derived.by(() => {
    if (!s || !s.wake_enabled) return '';
    if (!s.microphone_enabled) return t('voice.wake_needs_mic');
    if (s.wake_status === 'on' || s.wake_status === 'starting') return t('voice.wake_on', { name: s.wake_phrase });
    return s.wake_status;
  });

  async function applyWindows() {
    if (!s) return;
    try {
      await invoke('set_voice', { voice: s.voice, rate: s.rate });
    } catch (e) {
      fail(e);
    }
  }

  async function saveSpeech(): Promise<boolean> {
    if (!s) return false;
    try {
      await invoke('save_speech_settings', {
        tts: s.tts,
        ttsModel: s.tts_model,
        ttsVoice: s.tts_voice,
        sttModel: s.stt_model,
        sttLanguage: s.stt_language,
        baseUrl: s.speech_base_url,
        inputDevice: s.input_device,
        apiKey: key ? key : null,
      });
      if (key) s.has_speech_key = true;
      key = '';
      note = t('providers.saved');
      noteKind = 'ok';
      return true;
    } catch (e) {
      fail(e);
      return false;
    }
  }

  async function setEngine(engine: Engine) {
    if (!s) return;
    s.tts = engine;
    await saveSpeech();
  }

  async function testVoice() {
    await applyWindows();
    if (!(await saveSpeech())) return;
    note = '';
    invoke('test_voice', { text: t('voice.test_phrase', { name }) });
  }

  async function testMic() {
    if (listening) {
      // A second press ends the phrase early.
      invoke('test_microphone').catch(() => {});
      return;
    }
    if (!(await saveSpeech())) return;
    note = '';
    heard = '';
    listening = true;
    try {
      const text = await invoke<string>('test_microphone');
      if (text) heard = text;
    } catch (e) {
      if (String(e)) fail(e);
    } finally {
      listening = false;
    }
  }
</script>

{#if s}
  <Switch checked={s.speak_replies} title={t('voice.speak')} sub={t('voice.speak_sub')} onchange={setSpeak} />

  <div class="lbl top">{t('voice.engine')}</div>
  <div class="seg" role="radiogroup" aria-label={t('voice.engine')}>
    <button role="radio" aria-checked={s.tts === 'windows'} class:on={s.tts === 'windows'} onclick={() => setEngine('windows')}>
      {t('voice.engine_windows')}
    </button>
    <button role="radio" aria-checked={s.tts === 'provider'} class:on={s.tts === 'provider'} onclick={() => setEngine('provider')}>
      {t('voice.engine_provider')}
    </button>
  </div>

  {#if s.tts === 'windows'}
    {#if s.voices.length === 0}
      <p class="warn">{t('voice.none')}</p>
    {:else}
      <div class="lbl top">{t('voice.voice')}</div>
      <select class="fld" bind:value={s.voice} onchange={applyWindows}>
        <option value="">{t('voice.voice_auto')}</option>
        {#each s.voices as v (v.name)}
          <option value={v.name}>{v.name} · {v.language}</option>
        {/each}
      </select>
      {#if !s.has_russian}<p class="warn">{t('voice.no_russian')}</p>{/if}
      <div class="lbl top">{t('voice.rate')} · {s.rate.toFixed(1)}</div>
      <input class="rate" type="range" min="0.5" max="2" step="0.1" bind:value={s.rate} onchange={applyWindows} aria-label={t('voice.rate')} />
    {/if}
  {:else}
    <p class="hint">{t('voice.provider_sub')}</p>
    <label>
      <span class="lbl">{t('voice.tts_model')}</span>
      <input class="fld" bind:value={s.tts_model} placeholder="google/gemini-3.8-flash-lite-tts" spellcheck="false" />
    </label>
    <label>
      <span class="lbl">{t('voice.tts_voice')}</span>
      <input class="fld" bind:value={s.tts_voice} placeholder="Kore" spellcheck="false" />
    </label>
  {/if}
  <div class="acts">
    <button class="bt" onclick={testVoice}>{t('voice.test')}</button>
  </div>

  <div class="lbl sect">{t('voice.input')}</div>
  <Switch checked={s.microphone_enabled} title={t('voice.mic')} sub={t('voice.mic_sub', { shortcut: s.listen_shortcut })} onchange={setMic} />
  {#if !s.microphone_found}<p class="warn">{t('voice.mic_missing')}</p>{/if}
  {#if s.input_devices.length > 0}
    <label>
      <span class="lbl">{t('voice.device')}</span>
      <select class="fld" bind:value={s.input_device} onchange={saveSpeech}>
        <option value="">{t('voice.device_default')}</option>
        {#each s.input_devices as d (d)}
          <option value={d}>{d}</option>
        {/each}
      </select>
    </label>
  {/if}
  <Switch checked={s.wake_enabled} title={t('voice.wake', { name: s.wake_phrase })} sub={t('voice.wake_sub')} onchange={setWake} />
  <label>
    <span class="lbl">{t('voice.wake_phrase')}</span>
    <input class="fld short2" bind:value={s.wake_phrase} maxlength="40" onchange={() => s && setWake(s.wake_enabled)} />
  </label>
  {#if wakeNote}<p class="hint">{wakeNote}</p>{/if}
  <label>
    <span class="lbl">{t('voice.stt_model')}</span>
    <input class="fld" bind:value={s.stt_model} placeholder="qwen/qwen3-asr-0.6b" spellcheck="false" />
  </label>
  <label>
    <span class="lbl">{t('voice.stt_language')}</span>
    <input class="fld short" bind:value={s.stt_language} maxlength="8" placeholder="ru" spellcheck="false" />
  </label>

  <div class="lbl sect">{t('voice.provider')}</div>
  <label>
    <span class="lbl">{t('voice.base_url')}</span>
    <input class="fld" bind:value={s.speech_base_url} placeholder={s.llm_base_url} spellcheck="false" />
  </label>
  <label>
    <span class="lbl">{t('voice.key')}</span>
    <input class="fld" type="password" bind:value={key} placeholder={s.has_speech_key ? t('providers.key_saved') : ''} autocomplete="off" />
  </label>
  <div class="acts">
    <button class="bt pri" onclick={saveSpeech}>{t('providers.save')}</button>
    <button class="bt" onclick={testMic} disabled={!s.microphone_enabled || !s.stt_model.trim()}>
      {t(listening ? 'voice.mic_stop' : 'voice.mic_test')}
    </button>
  </div>
  {#if heard}<p class="heard">{t('voice.heard', { text: heard })}</p>{/if}
{/if}
{#if note}<p class="note {noteKind}">{note}</p>{/if}

<style>
  .top {
    margin-top: 18px;
  }
  .sect {
    margin-top: 26px;
    padding-top: 18px;
    border-top: 1px solid var(--line);
  }
  label {
    display: block;
  }
  label .lbl {
    display: block;
    margin: 12px 0 6px;
  }
  select.fld {
    appearance: none;
    cursor: pointer;
  }
  select.fld option {
    background: #14151f;
  }
  .short {
    width: 120px;
  }
  .short2 {
    width: 220px;
  }
  .rate {
    width: 100%;
    accent-color: var(--v);
  }
  .acts {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }
  .hint {
    margin: 12px 2px 0;
    font-size: 12.5px;
    color: var(--dim);
  }
  .warn {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--warn);
  }
  .heard {
    margin: 12px 2px 0;
    font-size: 13.5px;
    user-select: text;
  }
  .note {
    margin: 10px 2px 0;
    font-size: 12.5px;
    color: var(--mute);
    user-select: text;
  }
  .note.ok {
    color: var(--ok);
  }
  .note.bad {
    color: var(--bad);
  }
</style>
