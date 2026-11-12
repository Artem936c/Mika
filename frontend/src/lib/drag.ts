// Moving the island and the window with the mouse. The page only starts the
// native drag; where the window lands and what it turns into is decided in Rust.

import { getCurrentWindow } from '@tauri-apps/api/window';

/** Pixels the pointer must travel before a press becomes a drag, not a click. */
export const DRAG_THRESHOLD = 6;

/** Elements that handle their own presses: a drag never starts on them. */
const CONTROLS = 'button, a, input, textarea, select, label, [role="button"], .no-drag';

export function isDrag(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD;
}

export function onControl(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(CONTROLS) !== null;
}

function startDragging() {
  getCurrentWindow()
    .startDragging()
    .catch(() => {});
}

/**
 * Lets the element move the window. With `threshold`, a short press stays a
 * click and only a press that travels becomes a drag (the folded island opens
 * on click). A double click calls `ondouble`, for example to maximise.
 */
export interface DragOptions {
  threshold?: boolean;
  ondouble?: () => void;
  /** The region stays still, for example an open panel inside the island. */
  disabled?: boolean;
}

export function dragRegion(node: HTMLElement, options: DragOptions = {}) {
  let start: { x: number; y: number } | null = null;

  const down = (e: PointerEvent) => {
    if (options.disabled || e.button !== 0 || onControl(e.target)) return;
    if (options.threshold) {
      start = { x: e.clientX, y: e.clientY };
    } else {
      startDragging();
    }
  };
  const move = (e: PointerEvent) => {
    if (!start || (e.buttons & 1) === 0) {
      start = null;
      return;
    }
    if (isDrag(e.clientX - start.x, e.clientY - start.y)) {
      start = null;
      startDragging();
    }
  };
  const up = () => (start = null);
  const dbl = (e: MouseEvent) => {
    if (!options.disabled && !onControl(e.target)) options.ondouble?.();
  };

  node.addEventListener('pointerdown', down);
  node.addEventListener('pointermove', move);
  node.addEventListener('pointerup', up);
  node.addEventListener('dblclick', dbl);
  return {
    update(next: DragOptions) {
      options = next ?? {};
    },
    destroy() {
      node.removeEventListener('pointerdown', down);
      node.removeEventListener('pointermove', move);
      node.removeEventListener('pointerup', up);
      node.removeEventListener('dblclick', dbl);
    },
  };
}

export type Edge = 'North' | 'South' | 'East' | 'West' | 'NorthEast' | 'NorthWest' | 'SouthEast' | 'SouthWest';

export const EDGES: Edge[] = ['North', 'South', 'East', 'West', 'NorthEast', 'NorthWest', 'SouthEast', 'SouthWest'];

/** Starts resizing the ordinary window from one of its edges. */
export function resizeFrom(edge: Edge, e: PointerEvent) {
  if (e.button !== 0) return;
  e.preventDefault();
  getCurrentWindow()
    .startResizeDragging(edge)
    .catch(() => {});
}

export function toggleMaximize() {
  getCurrentWindow()
    .toggleMaximize()
    .catch(() => {});
}
