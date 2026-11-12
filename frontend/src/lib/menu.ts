// The browser's right-click menu. The app is not a web page, so it is off
// everywhere except text fields, where cut, copy and paste are useful.

const EDITABLE = 'input:not([type="checkbox"]):not([type="radio"]):not([type="range"]), textarea, [contenteditable=""], [contenteditable="true"]';

export function allowsContextMenu(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(EDITABLE) !== null;
}
