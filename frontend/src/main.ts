import { mount } from 'svelte';
import '@fontsource-variable/manrope';
import './app.css';
import './avatar.css';

const params = new URLSearchParams(location.search);
const scenario = params.get('mock');
if (import.meta.env.DEV && scenario !== null) {
  const { install } = await import('./dev/mock');
  await install(scenario);
}

// The desktop avatar window loads the same bundle with `?view=avatar`.
const { default: Root } =
  params.get('view') === 'avatar' ? await import('./AvatarWindow.svelte') : await import('./App.svelte');

const app = mount(Root, { target: document.getElementById('app')! });

export default app;
