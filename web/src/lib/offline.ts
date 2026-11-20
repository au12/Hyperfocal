// Offline copies through the service worker (public/sw.js). Browsers run service
// workers only in a secure context: HTTPS, or localhost.

export const offlineSupported = typeof navigator !== 'undefined' && 'serviceWorker' in navigator && typeof window !== 'undefined' && window.isSecureContext;

/** Turns the offline copies on (registers the worker) or off (removes it and them). */
export async function setOffline(on: boolean): Promise<void> {
  if (!offlineSupported) return;
  try {
    if (on) {
      await navigator.serviceWorker.register('./sw.js', { scope: './' });
    } else {
      for (const r of await navigator.serviceWorker.getRegistrations()) await r.unregister();
      await clearOffline();
    }
  } catch (e) {
    console.warn('offline copies:', e);
  }
}

/** Deletes every offline copy (the worker stays). */
export async function clearOffline(): Promise<void> {
  if (typeof caches === 'undefined') return;
  for (const k of await caches.keys()) if (k.startsWith('hf-')) await caches.delete(k);
}

/** Bytes this site keeps in the browser (offline copies, mostly), if the browser says. */
export async function offlineUsage(): Promise<number | null> {
  try {
    const e = await navigator.storage?.estimate?.();
    return e?.usage ?? null;
  } catch {
    return null;
  }
}
