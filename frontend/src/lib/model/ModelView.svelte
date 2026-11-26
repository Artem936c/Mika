<script lang="ts">
  // The living model in a full view. The scene and its code are loaded on
  // first use; while they load she is the picture drawn at import.
  import { invoke } from '@tauri-apps/api/core';
  import { untrack } from 'svelte';
  import type { Emotion } from '../avatar';
  import type { FailKind, Scene } from './scene';
  import { FULL_STILL, stillsKey, windowLabel } from './show';
  import { cursorWanted, live, releaseShownScene, setShownScene } from './live.svelte';
  import { model, onSceneDrop, reportFailure, reportStillsFailure } from './state.svelte';

  let {
    emotion,
    talking = false,
    pokes = 0,
  }: {
    emotion: Emotion;
    talking?: boolean;
    /** Grows by one with every click on her. */
    pokes?: number;
  } = $props();

  let host: HTMLElement | undefined = $state();
  let scene = $state.raw<Scene | null>(null);
  /** Her last frame, kept after the canvas went to another view. */
  let left = $state('');
  /** The canvas has come in over the picture. */
  let covered = $state(false);

  /** The desktop window always shows her whole; the chat column as the settings say. */
  const desktop = windowLabel(location.search) === 'avatar';

  const still = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;

  function fail(id: string, kind: FailKind | 'load') {
    reportFailure(kind);
    invoke('avatar_model_failed', { id, kind }).catch(() => {});
    import('./scene').then((m) => m.dropScene()).catch(() => {});
  }

  /** What the pictures on the disk were drawn for, as far as this view knows. */
  let drawnKey = '';

  /**
   * Draws the pictures of the model and gives them to Rust. A model brought
   * in a moment ago has none yet, and after a slot or the lip strength changes
   * the old ones show the old faces.
   */
  async function drawStills(s: Scene, id: string) {
    try {
      const stills = [...s.stills()].map(([name, url]) => ({ name, png_base64: url }));
      await invoke('avatar_model_save_stills', { id, stills });
    } catch {
      // The chips stay as they were; the page says so instead of waiting for ever.
      reportStillsFailure();
    }
  }

  // The scene of the chosen model: loaded once for the window, shared by its views.
  $effect(() => {
    const id = model.id;
    const node = host;
    if (!id || !node) return;
    const profile = untrack(() => model.profile);
    if (!profile) return;
    let gone = false;
    let mine: Scene | null = null;
    (async () => {
      try {
        const { sceneFor, dropScene } = await import('./scene');
        onSceneDrop(dropScene);
        const s = await sceneFor(id, profile);
        if (gone) return;
        mine = s;
        s.attach(node, {
          onLeave: (frame) => {
            left = frame;
            covered = false;
            scene = null;
          },
          onFail: (kind) => fail(id, kind),
        });
        // Pictures that are there are taken to match the profile they came with.
        drawnKey = stillsKey(profile);
        scene = s;
        setShownScene(s, node);
        // The picture stays under the canvas while it comes in, then goes: she is see-through around the edges.
        // Not when another view took the canvas meanwhile: then the picture is all this view has.
        setTimeout(() => {
          if (!gone && scene === s) {
            left = '';
            covered = true;
          }
        }, 260);
        if (!untrack(() => model.stillsComplete)) drawStills(s, id);
      } catch (e) {
        if (gone) return;
        const kind = (e as { kind?: FailKind })?.kind ?? 'load';
        fail(id, kind);
      }
    })();
    return () => {
      gone = true;
      mine?.detach(node);
      // A view that came in since keeps the scene.
      releaseShownScene(node);
      scene = null;
      covered = false;
    };
  });

  // A slot or the lip strength changed: the same scene, the newer profile,
  // and a little later new pictures for the chips.
  $effect(() => {
    const profile = model.profile;
    const s = scene;
    if (!s || !profile) return;
    s.setProfile(profile);
    const key = stillsKey(profile);
    if (key === drawnKey) return;
    const id = untrack(() => model.id);
    const timer = setTimeout(() => {
      drawnKey = key;
      drawStills(s, id);
    }, 700);
    return () => clearTimeout(timer);
  });

  // The cursor comes from Rust only while a model here wants it: on the
  // desktop always, to tell clicks on her from clicks past her.
  $effect(() => {
    // The desktop window asks for the cursor itself, for the clicks too.
    if (desktop || scene === null || !model.look.follow_cursor) return;
    // Views of a window are counted: the one that leaves does not take the cursor from the one that came.
    const tell = (on: boolean | null) => {
      if (on !== null) invoke('avatar_cursor_wanted', { on }).catch(() => {});
    };
    tell(cursorWanted(1));
    return () => tell(cursorWanted(-1));
  });

  $effect(() => {
    scene?.set({
      emotion,
      talking,
      windowShown: model.windowShown,
      still,
      state: live.state,
      working: live.working,
      sentAt: live.sentAt,
      pose: model.look.pose,
      persona: model.look.persona,
      idleActions: model.look.idle_actions,
      idleMotion: model.look.idle_motion || null,
      frame: desktop ? 'full' : model.look.frame,
      follow: model.look.follow_cursor,
      cursor: live.cursor,
      typing: live.typing,
    });
  });

  // The body moves once when a new emotion begins, and hops when she is clicked.
  let shown: Emotion | null = null;
  $effect(() => {
    const e = emotion;
    if (shown !== null && shown !== e) untrack(() => scene)?.gesture(e);
    shown = e;
  });
  let poked = 0;
  $effect(() => {
    const n = pokes;
    if (n > poked) untrack(() => scene)?.gesture('poke');
    poked = n;
  });

  const waiting = $derived(left || (covered ? '' : (model.stills.get(FULL_STILL) ?? '')));
</script>

<span class="mview" class:ready={scene !== null} bind:this={host}>
  {#if waiting}<img class="still" src={waiting} alt="" draggable="false" />{/if}
</span>

<style>
  .mview {
    display: block;
    position: relative;
    width: 100%;
    aspect-ratio: 3 / 4;
  }
  .still {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
    pointer-events: none;
  }
  /* The canvas the scene puts here comes in softly over the picture. */
  .mview :global(canvas) {
    animation: come 0.25s ease-out;
  }
  @keyframes come {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .mview :global(canvas) {
      animation: none;
    }
  }
</style>
