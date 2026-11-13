// Keeping an installed phone page up to date.
//
// The page is cached by a service worker, which is what lets it open with no
// network at all — and the same cache is what would keep an old build forever if
// nothing asked for a new one. So:
//
// - the worker is re-checked when the page becomes visible, when the network
//   comes back, and once a minute while it is in front of the person;
// - when a new worker takes control, the page reloads itself, unless a recording
//   is in flight — then the reader is asked instead of losing the take.
//
// The daemon serves `sw.js` with `Cache-Control: no-store`, so a new build is
// always visible to this check.

export interface UpdateOptions {
  /** Called when a new build is installed but reloading now would lose work. */
  onUpdateReady: () => void;
  /** True when nothing is in flight and the page may reload immediately. */
  mayReload: () => boolean;
}

const CHECK_EVERY_MS = 60_000;

export function startUpdates(options: UpdateOptions): void {
  if (!import.meta.env.PROD || !('serviceWorker' in navigator)) return;

  // On the very first visit there is no controller; the worker then claims the
  // page and fires `controllerchange`. That is an install, not an update, and it
  // must not reload the page.
  const controlledAtStart = navigator.serviceWorker.controller !== null;
  let reloading = false;

  navigator.serviceWorker.addEventListener('controllerchange', () => {
    if (reloading || !controlledAtStart) return;
    if (!options.mayReload()) {
      options.onUpdateReady();
      return;
    }
    reloading = true;
    location.reload();
  });

  navigator.serviceWorker
    .register('./sw.js')
    .then((registration) => {
      const check = (): void => {
        registration.update().catch(() => {
          // A failed check costs nothing: the cached page still works, and the
          // next check may succeed.
        });
      };
      check();
      window.setInterval(() => {
        if (!document.hidden) check();
      }, CHECK_EVERY_MS);
      document.addEventListener('visibilitychange', () => {
        if (!document.hidden) check();
      });
      window.addEventListener('online', check);
    })
    .catch((error: unknown) => {
      const trace = (window as unknown as { app?: { trace?: (line: string) => void } }).app
        ?.trace;
      if (trace) trace('service worker failed: ' + String(error));
    });
}
