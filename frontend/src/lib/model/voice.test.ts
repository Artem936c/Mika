import { describe, expect, it } from 'vitest';
import { mouthNow, onVoiceEvent, talkingNow, voice } from './voice.svelte';

describe('voice of the lips', () => {
  it('moves the mouth while a phrase sounds and stops on quiet', () => {
    onVoiceEvent({ type: 'voice_phrase', text: 'аа', frame_ms: 20, levels: [255, 255] }, 1000);
    expect(voice.playing).toBe(true);
    expect(talkingNow(false)).toBe(true);
    expect(mouthNow(1010)?.shape).toBe('aa');
    onVoiceEvent({ type: 'voice_quiet' });
    expect(voice.playing).toBe(false);
    expect(mouthNow(1010)).toBe(null);
  });

  it('does not move the mouth for a written reply while the voice is on', () => {
    onVoiceEvent({ type: 'voice_quiet' });
    expect(talkingNow(true)).toBe(false);
  });

  it('lets a phrase go when its end never came', () => {
    onVoiceEvent({ type: 'voice_phrase', text: 'а', frame_ms: 20, levels: [255] }, 0);
    expect(mouthNow(10)).not.toBe(null);
    expect(mouthNow(5000)).toBe(null);
    expect(voice.playing).toBe(false);
  });

  it('runs an estimate in a loop for a phrase without loudness', () => {
    onVoiceEvent({ type: 'voice_phrase', text: 'а', frame_ms: 20, levels: [] }, 0);
    expect(mouthNow(10)?.shape).toBe('aa');
    expect(mouthNow(100_000)?.shape).toBe('aa');
    onVoiceEvent({ type: 'voice_quiet' });
  });
});
