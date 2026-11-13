<script lang="ts">
  import ModelPicker from './ModelPicker.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen as onEvent } from '@tauri-apps/api/event';
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
    tts_cache: boolean;
    tts_cache_bytes: number;
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
    vosk_installed: boolean;
    vosk_downloading: boolean;
    vosk_bytes: number;
    follow_up: boolean;
    duck_media: boolean;
  }

  let { name }: { name: string } = $props();

  let s = $state<VoiceSettings | null>(null);
  let key = $state('');
  let note = $state('');
  let noteKind = $state<'ok' | 'bad' | ''>('');
  let heard = $state('');
  let listening = $state(false);

  let voskShare = $state(0);

  onMount(() => {
    invoke<VoiceSettings>('get_voice_settings').then((v) => (s = v), fail);
    const off = onEvent<number>('vosk-progress', (e) => (voskShare = e.payload));
    return () => {
      off.then((f) => f());
    };
  });

  async function setFollowUp(v: boolean) {
    if (!s) return;
    try {
      await invoke('set_follow_up', { value: v });
      s.follow_up = v;
    } catch (e) {
      fail(e);
    }
  }

  async function setDuck(v: boolean) {
    if (!s) return;
    try {
      await invoke('set_duck_media', { value: v });
      s.duck_media = v;
    } catch (e) {
      fail(e);
    }
  }

  async function installVosk() {
    if (!s) return;
    s.vosk_downloading = true;
    voskShare = 0;
    try {
      s.wake_status = await invoke<string>('install_vosk');
      s = await invoke<VoiceSettings>('get_voice_settings');
    } catch (e) {
      fail(e);
      if (s) s.vosk_downloading = false;
    }
  }

  async function removeVosk() {
    if (!s) return;
    try {
      s.wake_status = await invoke<string>('remove_vosk');
      s = await invoke<VoiceSettings>('get_voice_settings');
    } catch (e) {
      fail(e);
    }
  }

  const voskSize = $derived(s ? Math.round(s.vosk_bytes / (1024 * 1024)) : 0);

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
    if (s.wake_status === 'on' || s.wake_status === 'starting') {
      return t(s.vosk_installed ? 'voice.wake_on_vosk' : 'voice.wake_on', { name: s.wake_phrase });
    }
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

  async function setCache(v: boolean) {
    if (!s) return;
    try {
      await invoke('set_tts_cache', { value: v });
      s.tts_cache = v;
    } catch (e) {
      fail(e);
    }
  }

  async function clearCache() {
    if (!s) return;
    try {
      s.tts_cache_bytes = await invoke<number>('clear_speech_cache');
    } catch (e) {
      fail(e);
    }
  }

  const cacheSize = $derived(s ? (s.tts_cache_bytes / (1024 * 1024)).toFixed(1) : '0');

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

  interface VoiceList {
    voices: string[];
    source: 'provider' | 'known' | 'none';
  }
  let voicesOpen = $state(false);
  let voices = $state<VoiceList | null>(null);

  async function toggleVoices() {
    voicesOpen = !voicesOpen;
    if (!voicesOpen || !s) return;
    voices = null;
    try {
      voices = await invoke<VoiceList>('list_voices', { model: s.tts_model, baseUrl: s.speech_base_url || null });
    } catch (e) {
      voicesOpen = false;
      fail(e);
    }
  }

  async function listen(v: string) {
    if (!s) return;
    s.tts_voice = v;
    await testVoice();
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
    <div class="lab">
      <span class="lbl">{t('voice.tts_model')}</span>
      <ModelPicker bind:value={s.tts_model} purpose="tts" baseUrl={s.speech_base_url} placeholder="google/gemini-3.8-flash-lite-tts" />
    </div>
    <div class="lab">
      <span class="lbl">{t('voice.tts_voice')}</span>
      <div class="vrow">
        <input class="fld" bind:value={s.tts_voice} placeholder="Kore" spellcheck="false" />
        <button class="bt" class:on={voicesOpen} onclick={toggleVoices}>{t('voices.browse')}</button>
      </div>
      {#if voicesOpen}
        {#if voices === null}
          <p class="hint">{t('models.loading')}</p>
        {:else}
          <p class="hint">{t(`voices.${voices.source}`)}</p>
          <div class="voices">
            {#each voices.voices as v (v)}
              <span class="vc" class:on={s.tts_voice === v}>
                <button class="vname" onclick={() => s && (s.tts_voice = v)}>{v}</button>
                <button class="vplay" title={t('voices.listen')} aria-label={t('voices.listen')} onclick={() => listen(v)}>▶</button>
              </span>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
    <Switch checked={s.tts_cache} title={t('voice.cache')} sub={t('voice.cache_sub', { size: cacheSize })} onchange={setCache} />
  {/if}
  <div class="acts">
    <button class="bt" onclick={testVoice}>{t('voice.test')}</button>
    {#if s.tts === 'provider' && s.tts_cache_bytes > 0}
      <button class="bt" onclick={clearCache}>{t('voice.cache_clear')}</button>
    {/if}
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
  <Switch checked={s.wake_enabled} title={t('voice.wake', { name: s.wake_phrase })} sub={t('voice.wake_sub', { name: s.wake_phrase })} onchange={setWake} />
  <label>
    <span class="lbl">{t('voice.wake_phrase')}</span>
    <input class="fld short2" bind:value={s.wake_phrase} maxlength="40" onchange={() => s && setWake(s.wake_enabled)} />
  </label>
  {#if wakeNote}<p class="hint">{wakeNote}</p>{/if}
  <Switch checked={s.follow_up} title={t('voice.follow_up')} sub={t('voice.follow_up_sub')} onchange={setFollowUp} />
  <Switch checked={s.duck_media} title={t('voice.duck')} sub={t('voice.duck_sub')} onchange={setDuck} />
  <div class="lbl top">{t('voice.vosk')}</div>
  {#if s.vosk_downloading}
    <p class="hint">{t('voice.vosk_loading', { share: voskShare })}</p>
  {:else if s.vosk_installed}
    <p class="hint">{t('voice.vosk_on', { size: voskSize })}</p>
    <div class="acts">
      <button class="bt" onclick={removeVosk}>{t('voice.vosk_remove')}</button>
    </div>
  {:else}
    <p class="hint">{t('voice.vosk_off')}</p>
    <div class="acts">
      <button class="bt" onclick={installVosk}>{t('voice.vosk_get')}</button>
    </div>
  {/if}
  <div class="lab">
    <span class="lbl">{t('voice.stt_model')}</span>
    <ModelPicker bind:value={s.stt_model} purpose="stt" baseUrl={s.speech_base_url} placeholder="qwen/qwen3-asr-0.6b" />
  </div>
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
  label,
  .lab {
    display: block;
  }
  label .lbl,
  .lab .lbl {
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
  .vrow {
    display: flex;
    gap: 8px;
  }
  .vrow .fld {
    flex: 1;
    min-width: 0;
  }
  .bt.on {
    border-color: rgba(143, 125, 255, 0.55);
  }
  .voices {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
    max-height: 180px;
    overflow-y: auto;
  }
  .vc {
    display: inline-flex;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    overflow: hidden;
  }
  .vc.on {
    border-color: rgba(143, 125, 255, 0.6);
    background: rgba(143, 125, 255, 0.14);
  }
  .vc button {
    all: unset;
    cursor: pointer;
    padding: 5px 9px;
    font-size: 12.5px;
  }
  .vc .vplay {
    padding: 5px 8px;
    color: var(--mute);
    border-left: 1px solid var(--line);
  }
  .vc button:hover {
    background: rgba(255, 255, 255, 0.06);
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
