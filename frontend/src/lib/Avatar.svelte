<script lang="ts">
  import { avatarSvg, type AvatarView, type Emotion } from './avatar';

  let {
    view = 'full',
    emotion = 'neutral',
    talking = false,
    size,
  }: {
    view?: AvatarView;
    emotion?: Emotion;
    /** Moves the lips. */
    talking?: boolean;
    /** Round chip of this diameter, px. Without it the avatar fills its parent's width. */
    size?: number;
  } = $props();

  const svg = $derived(avatarSvg(view, emotion));
</script>

<!-- The markup comes from avatar.ts and holds no outside data. -->
<span
  class="avatar"
  class:chip={size !== undefined}
  class:talking
  style:width={size !== undefined ? `${size}px` : undefined}
  style:height={size !== undefined ? `${size}px` : undefined}
>
  {@html svg}
</span>

<style>
  .avatar {
    display: block;
  }
  .chip {
    flex: none;
    border-radius: 50%;
    overflow: hidden;
    background: radial-gradient(circle at 50% 30%, #4a4090, #1b1736);
    box-shadow: 0 0 0 1.5px rgba(255, 255, 255, 0.14);
  }
</style>
