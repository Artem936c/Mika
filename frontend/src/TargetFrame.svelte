<script lang="ts">
  // A frame around the element the assistant is about to click.
  // The window itself is sized and placed by Rust; this only draws the border.
</script>

<div class="frame"></div>

<style>
  .frame {
    position: fixed;
    inset: 0;
    box-sizing: border-box;
    border: 3px solid var(--c);
    border-radius: 9px;
    box-shadow:
      0 0 14px rgba(var(--c-rgb), 0.8),
      inset 0 0 10px rgba(var(--c-rgb), 0.35);
    animation: pulse 0.45s ease-in-out infinite alternate;
  }
  @keyframes pulse {
    to {
      border-color: var(--p);
      box-shadow:
        0 0 14px rgba(var(--p-rgb), 0.8),
        inset 0 0 10px rgba(var(--p-rgb), 0.35);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .frame {
      animation: none;
    }
  }
</style>
