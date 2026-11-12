import { mount } from 'svelte';
import '@fontsource-variable/manrope';
import './app.css';
import './avatar.css';
import { allowsContextMenu } from './lib/menu';

const params = new URLSearchParams(location.search);
const scenario = params.get('mock');
if (import.meta.env.DEV && scenario !== null) {
  const { install } = await import('./dev/mock');
  await install(scenario);
}

// The desktop avatar window loads the same bundle with `?view=avatar`.
// The click-target frame loads it with `?view=target`.
const view = params.get('view');
const { default: Root } =
  view === 'avatar'
    ? await import('./AvatarWindow.svelte')
    : view === 'target'
      ? await import('./TargetFrame.svelte')
      : await import('./App.svelte');

// No browser menu on right click: the app is not a web page. Text fields keep theirs
// for cut, copy and paste.
document.addEventListener('contextmenu', (e) => {
  if (!allowsContextMenu(e.target)) e.preventDefault();
});

const app = mount(Root, { target: document.getElementById('app')! });

export default app;
