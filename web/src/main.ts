import { mount } from 'svelte';
import App from './App.svelte';
import { loadLanguage } from './i18n';
import { applyTheme, readTheme } from './lib/theme';
import { takeTokenFromHash } from './lib/token';
import { startUpdates } from './lib/updates';
import { page } from './state.svelte';
import './app.css';

/** Give up on a fetch that hangs, so a phone always gets its page. */
function deadline(work: Promise<unknown>, ms: number): Promise<void> {
  const settled = work.then(
    () => undefined,
    () => undefined,
  );
  const expired = new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });
  return Promise.race([settled, expired]);
}

// A pairing token may arrive in the fragment when the page is moved between
// addresses. It is stored and stripped before anything asks the computer.
takeTokenFromHash();

// The saved mode is applied before the first paint; the page never flashes the
// wrong palette.
applyTheme(readTheme());

// The language decides the first paint (the page must not flash raw keys).
// The version is baked into the bundle, so it needs no network at all.
await deadline(loadLanguage(), 3000);

const target = document.getElementById('app');
if (!target) {
  throw new Error('Missing #app mount point in index.html');
}

mount(App, { target });

// A new build reaches the phone on its own: the worker is re-checked while the
// page is open, and the page reloads when it takes over — unless a recording is
// in flight, in which case the footer offers the update instead.
startUpdates({
  onUpdateReady: () => page.markUpdateReady(),
  mayReload: () => !page.recording,
});
