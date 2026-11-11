import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

const scenario = new URLSearchParams(location.search).get('mock');
if (import.meta.env.DEV && scenario !== null) {
  const { install } = await import('./dev/mock');
  await install(scenario);
}

const app = mount(App, { target: document.getElementById('app')! });

export default app;
