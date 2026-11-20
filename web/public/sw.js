// Hyperfocal service worker: the app and the photos looked at stay available offline.
//
// - Thumbnails and previews (versioned URLs, immutable): cache first, kept up to a
//   limit, oldest dropped first. Never a response marked no-store (a HEIC file's
//   draft thumbnail, replaced once the real one is made).
// - The app's JSON (folders, photo lists, details...): network first, the last copy
//   when the server cannot be reached, marked with an x-hf-offline header so the app
//   can say it is showing saved copies.
// - The app itself: hashed assets cache first, the page network first.
// - Everything else (RAW renders and tiles, live events, changes): straight through.

const VERSION = 'v1';
const SHELL = `hf-shell-${VERSION}`;
const MEDIA = `hf-media-${VERSION}`;
const DATA = `hf-data-${VERSION}`;
/** Entries kept per cache (thumbnails are ~20-60 KB, previews ~0.3-1 MB). */
const LIMIT = { [MEDIA]: 6000, [DATA]: 400, [SHELL]: 80 };
/** JSON larger than this is not kept (a whole-library list can run to megabytes). */
const MAX_JSON = 8 * 1024 * 1024;

const scope = new URL(self.registration.scope);
const apiRoot = scope.pathname + 'api/';
const MEDIA_RE = /^photo\/\d+\/(thumb|preview)$/;
const DATA_RE = /^(status|tree|photos|timeline|collections|facets|keywords|keywords\/tree|geo\/clusters|photo\/\d+)$/;

self.addEventListener('install', () => self.skipWaiting());

self.addEventListener('activate', (e) => {
  e.waitUntil(
    (async () => {
      for (const k of await caches.keys()) if (k.startsWith('hf-') && ![SHELL, MEDIA, DATA].includes(k)) await caches.delete(k);
      await self.clients.claim();
    })(),
  );
});

self.addEventListener('message', (e) => {
  if (e.data === 'clear') {
    e.waitUntil(
      (async () => {
        for (const k of await caches.keys()) if (k.startsWith('hf-')) await caches.delete(k);
      })(),
    );
  }
});

self.addEventListener('fetch', (e) => {
  const req = e.request;
  if (req.method !== 'GET' || req.headers.has('range')) return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith(apiRoot)) {
    const rest = url.pathname.slice(apiRoot.length);
    if (MEDIA_RE.test(rest) && url.searchParams.has('v')) e.respondWith(media(req));
    else if (DATA_RE.test(rest) && !url.searchParams.has('count')) e.respondWith(networkFirst(req, DATA));
    return;
  }
  if (req.mode === 'navigate') {
    // A single page: whatever the path, the page is the one at the scope.
    e.respondWith(networkFirst(req, SHELL, scope.href));
    return;
  }
  if (url.pathname.startsWith(scope.pathname + 'assets/') || /\.(woff2|png|webmanifest)$/.test(url.pathname)) {
    e.respondWith(cacheFirst(req, SHELL));
  }
});

const noStore = (res) => /no-store/i.test(res.headers.get('cache-control') || '');

let puts = 0;
function remember(name, key, res) {
  return caches
    .open(name)
    .then((c) => c.put(key, res))
    .then(() => {
      if (++puts % 40 === 0) trimAll();
    })
    .catch(() => {});
}

async function trimAll() {
  for (const [name, max] of Object.entries(LIMIT)) {
    const c = await caches.open(name);
    const keys = await c.keys();
    // Insertion order: the oldest first.
    for (let i = 0; i < keys.length - max; i++) await c.delete(keys[i]);
  }
}

async function media(req) {
  const c = await caches.open(MEDIA);
  const hit = await c.match(req);
  if (hit) return hit;
  const res = await fetch(req);
  if (res.status === 200 && !noStore(res)) remember(MEDIA, req, res.clone());
  return res;
}

async function cacheFirst(req, name) {
  const c = await caches.open(name);
  const hit = await c.match(req);
  if (hit) return hit;
  const res = await fetch(req);
  if (res.status === 200 && !noStore(res)) remember(name, req, res.clone());
  return res;
}

async function networkFirst(req, name, key) {
  try {
    const res = await fetch(req);
    if (res.status === 200 && !noStore(res)) {
      const copy = res.clone();
      // Skip very large JSON (read in the background; the page has its own copy).
      copy
        .blob()
        .then((b) => {
          if (b.size > MAX_JSON) return;
          // The body is already decoded: drop the transfer headers.
          const headers = new Headers(copy.headers);
          headers.delete('content-encoding');
          headers.delete('content-length');
          remember(name, key ?? req, new Response(b, { status: 200, statusText: copy.statusText, headers }));
        })
        .catch(() => {});
    }
    return res;
  } catch (err) {
    const hit = await (await caches.open(name)).match(key ?? req);
    if (!hit) throw err;
    const headers = new Headers(hit.headers);
    headers.set('x-hf-offline', '1');
    return new Response(hit.body, { status: hit.status, statusText: hit.statusText, headers });
  }
}
