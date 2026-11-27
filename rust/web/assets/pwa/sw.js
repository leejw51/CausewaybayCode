// The service worker: what makes the page a progressive web app. It never serves anything
// stale. Every request for a file of the page goes to the backend first; what came back is
// kept, and is used only when the backend cannot be reached. Calls to the backend's API are
// not touched, and neither is opening the page itself: a page shown from the copy would hide
// what the browser has to say when it cannot reach the backend, above all its question about
// a certificate it does not trust yet (the copy opened, every call behind it was refused,
// and there was nowhere to say "visit this website").
const CACHE = 'cwbcode-shell-v2';

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
  if (request.method !== 'GET' || request.mode === 'navigate' || url.origin !== self.location.origin || url.pathname.startsWith('/api/') || url.pathname.startsWith('/__diag/')) return;
  event.respondWith((async () => {
    const cache = await caches.open(CACHE);
    try {
      const fresh = await fetch(request);
      if (fresh.ok) cache.put(request, fresh.clone()).catch(() => {});
      return fresh;
    } catch (unreachable) {
      // The backend is away: the copy kept from last time.
      const kept = await cache.match(request);
      if (kept) return kept;
      throw unreachable;
    }
  })());
});
