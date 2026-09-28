import { mount } from 'svelte';
import '../lib/theme.css';
import { inApp, setMock } from '../lib/ipc';
import App from './App.svelte';

async function start() {
  if (!inApp) {
    // Plain browser (npm run dev): fake backend with sample data.
    setMock((await import('./mock')).mock);
  }
  mount(App, { target: document.getElementById('app')! });
}

start();
