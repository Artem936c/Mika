// Crossfade of island content, timings from the mockup:
// the old layer blurs out in 220 ms, the new one comes in after 160 ms.

import { cubicOut } from 'svelte/easing';
import type { TransitionConfig } from 'svelte/transition';

export function layerIn(_node: Element): TransitionConfig {
  return {
    delay: 160,
    duration: 400,
    easing: cubicOut,
    css: (t) => `opacity:${Math.min(1, t * 1.8)};filter:blur(${(1 - t) * 10}px);scale:${0.96 + 0.04 * t}`,
  };
}

export function layerOut(_node: Element): TransitionConfig {
  return {
    duration: 220,
    css: (t) => `opacity:${t};filter:blur(${(1 - t) * 10}px);pointer-events:none`,
  };
}
