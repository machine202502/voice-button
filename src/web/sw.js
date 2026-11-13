// The shell cache: enough to open the app with no network at all. Nothing that
// belongs to a person is cached here; messages and audio are IndexedDB.
//
// The fallback rule matters: a missing module must fail as a module, never be
// answered with the HTML page. Returning `index.html` for `js/app.js` made the
// browser refuse the module ("text/html") and the app showed a black square.

const SHELL = 'voice-button-shell-v32';
const FILES = [
  './',
  'css/styles.css',
  'js/app.js',
  'js/storage.mjs',
  'js/cache.mjs',
  'js/outbox.mjs',
  'js/sync.mjs',
  'js/loop.mjs',
  'js/backoff.mjs',
  'js/stage.mjs',
  'js/offline.mjs',
  'js/version.mjs',
  'js/install.mjs',
  'js/back.mjs',
  'js/chat.mjs',
  'js/scroll.mjs',
  'manifest.en.webmanifest',
  'manifest.ru.webmanifest',
  'manifest.zh.webmanifest',
  'lang/index.json',
  'lang/install.en.json',
  'lang/install.ru.json',
  'lang/install.zh.json',
  'lang/en.json',
  'lang/ru.json',
  'lang/zh.json',
  'icons/icon-192.png',
  'icons/icon-512.png',
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(SHELL)
      .then((cache) => cache.addAll(FILES))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== SHELL).map((key) => caches.delete(key))),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (
    request.method !== 'GET' ||
    url.pathname.startsWith('/site/api/') ||
    url.pathname === '/pair' ||
    url.pathname.endsWith('/version.json')
  ) {
    return;
  }
  const navigation = request.mode === 'navigate';
  event.respondWith(
    caches.match(request).then((cached) => {
      if (cached) return cached;
      return fetch(request).catch(() =>
        // Only a page navigation may fall back to the app shell. A script that
        // cannot be loaded must fail as a script; the page then reports the
        // problem instead of rendering a black square.
        navigation ? caches.match('./') : Response.error(),
      );
    }),
  );
});
