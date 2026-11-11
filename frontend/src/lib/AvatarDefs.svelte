<script lang="ts">
  // No logic: only shared SVG definitions.
</script>

<!-- Gradients and clip paths shared by every avatar on the page. Mount once per window. -->
<svg width="0" height="0" style="position:absolute" aria-hidden="true">
  <defs>
    <linearGradient id="mk-hair" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#f7f4ff" /><stop offset="1" stop-color="#d6cdfb" /></linearGradient>
    <linearGradient id="mk-tail" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ece7ff" /><stop offset=".6" stop-color="#c5baf6" /><stop offset="1" stop-color="#9fe8f7" /></linearGradient>
    <linearGradient id="mk-back" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#aca1ea" /><stop offset="1" stop-color="#8477d0" /></linearGradient>
    <linearGradient id="mk-skin" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff4ee" /><stop offset="1" stop-color="#ffe2d7" /></linearGradient>
    <linearGradient id="mk-iris" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4a1a6e" /><stop offset=".55" stop-color="#d0469f" /><stop offset="1" stop-color="#ffc1dc" /></linearGradient>
    <linearGradient id="mk-collar" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#343a6c" /><stop offset="1" stop-color="#1e2245" /></linearGradient>
    <clipPath id="mk-face"><path d="M132 190C130 140 160 104 200 104s70 36 68 86c0 36-14 66-36 84-14 12-24 18-32 18s-18-6-32-18c-22-18-36-48-36-84Z" /></clipPath>
    <clipPath id="mk-eye-l"><ellipse cx="166" cy="214" rx="20" ry="22" /></clipPath>
    <clipPath id="mk-eye-r"><ellipse cx="234" cy="214" rx="20" ry="22" /></clipPath>
  </defs>
</svg>
