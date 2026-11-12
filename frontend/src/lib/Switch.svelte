<script lang="ts">
  let {
    checked,
    title,
    sub,
    onchange,
  }: { checked: boolean; title: string; sub: string; onchange: (v: boolean) => void } = $props();
</script>

<label class="switch">
  <input type="checkbox" {checked} onchange={(e) => onchange(e.currentTarget.checked)} />
  <span class="knob" aria-hidden="true"></span>
  <span class="text"><b>{title}</b>{sub}</span>
</label>

<style>
  .switch {
    position: relative;
    display: flex;
    gap: 14px;
    align-items: flex-start;
    padding: 14px 16px;
    border-radius: 16px;
    border: 1px solid var(--line);
    background: rgba(255, 255, 255, 0.03);
    cursor: pointer;
  }
  /* The hidden box sits inside the switch: when a click focuses it, nothing has to
     scroll into view (an outer box with hidden overflow would shift the whole panel). */
  input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    pointer-events: none;
  }
  .knob {
    flex: none;
    width: 38px;
    height: 22px;
    margin-top: 1px;
    border-radius: 11px;
    background: rgba(255, 255, 255, 0.12);
    position: relative;
    transition: background 0.2s;
  }
  .knob::after {
    content: '';
    position: absolute;
    left: 3px;
    top: 3px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    transition: translate 0.25s var(--spring);
  }
  input:checked + .knob {
    background: linear-gradient(90deg, var(--v), var(--p));
  }
  input:checked + .knob::after {
    translate: 16px 0;
  }
  input:focus-visible + .knob {
    outline: 2px solid rgba(143, 125, 255, 0.7);
    outline-offset: 2px;
  }
  .text {
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--mute);
  }
  .text b {
    display: block;
    font-size: 14px;
    color: var(--ink);
    margin-bottom: 2px;
  }
</style>
