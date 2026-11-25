// The service worker: what makes the page a progressive web app. It never serves anything
// stale. Every request goes to the backend first; what came back is kept, and is used only
// when the backend cannot be reached, so that the app still opens (and says it is offline)
// instead of the browser's error page. Calls to the backend's API are not touched.
const CACHE = 'cwbcode-shell-v1';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const name of await caches.keys()) if (name !== CACHE) await caches.delete(name);
    await self.clients.claim();
  })());
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== self.location.origin || url.pathname.startsWith('/api/') || url.pathname.startsWith('/__diag/')) return;
  event.respondWith((async () => {
    const cache = await caches.open(CACHE);
    try {
      const fresh = await fetch(request);
      if (fresh.ok) cache.put(request, fresh.clone()).catch(() => {});
      return fresh;
    } catch (unreachable) {
      // The backend is away: the copy kept from last time, or the page itself for a route of the app.
      const kept = (await cache.match(request)) || (request.mode === 'navigate' ? await cache.match('/') : null);
      if (kept) return kept;
      throw unreachable;
    }
  })());
});
