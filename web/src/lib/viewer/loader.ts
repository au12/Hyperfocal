// Decoded-image cache for the single view: fetches with priorities, decodes off the
// main thread (createImageBitmap) and keeps the most recent few bitmaps.
//
// `slots` caps the loads running at once: the rest wait their turn, high priority first,
// then in the order asked. Cancelling a load that is still waiting costs nothing; a
// running one is aborted, which over HTTP/1.1 also closes its connection, so for small
// images (thumbnails) `cancelOthers` can leave the running ones to finish.

import { tel } from '../telemetry';

export interface Decoded {
  bmp: ImageBitmap;
  w: number;
  h: number;
  url: string;
}

interface Entry {
  p: Promise<Decoded>;
  ctl: AbortController;
  done?: Decoded;
  low: boolean;
  t: number;
  /** When it was asked for: waiting loads start in this order (within a priority). */
  seq: number;
  /** Set while it waits for a slot: starts it. */
  start?: () => void;
}

const aborted = () => new DOMException('Cancelled', 'AbortError');

/** Every cache, for debug mode's count of decoded images held in memory. */
const caches = new Set<WeakRef<ImageCache>>();

function held(): { n: number; bytes: number } {
  let n = 0;
  let bytes = 0;
  for (const r of caches) {
    const c = r.deref();
    if (!c) caches.delete(r);
    else {
      const s = c.held();
      n += s.n;
      bytes += s.bytes;
    }
  }
  return { n, bytes };
}
tel.gauge('bitmaps', () => held().n);
tel.gauge('bitmaps_mb', () => held().bytes / 1048576);

/** What a URL loads, for timings: raw, preview, jpeg, thumb… */
function kindOf(url: string): string {
  return url.match(/\/photo\/\d+\/([a-z]+)/)?.[1] ?? 'other';
}

export class ImageCache {
  private m = new Map<string, Entry>();
  private tick = 0;
  private running = 0;
  private maxTex = 16384;

  constructor(
    private max = 12,
    private slots = Infinity,
  ) {
    caches.add(new WeakRef(this));
  }

  /** Decoded images held, and their memory (4 bytes a pixel). */
  held(): { n: number; bytes: number } {
    let n = 0;
    let bytes = 0;
    for (const e of this.m.values()) {
      if (e.done) {
        n++;
        bytes += e.done.w * e.done.h * 4;
      }
    }
    return { n, bytes };
  }

  setMaxTexture(n: number) {
    this.maxTex = n;
  }

  peek(url: string): Decoded | undefined {
    const e = this.m.get(url);
    if (e) e.t = ++this.tick;
    return e?.done;
  }

  get(url: string, low = false): Promise<Decoded> {
    const hit = this.m.get(url);
    if (hit) {
      hit.t = ++this.tick;
      if (!low && hit.low && !hit.done) {
        hit.low = false;
        this.pump();
      }
      return hit.p;
    }
    const ctl = new AbortController();
    const entry = { ctl, low, t: ++this.tick, seq: this.tick } as Entry;
    entry.p = (async () => {
      await this.slot(entry);
      try {
        return await this.load(url, entry);
      } finally {
        this.running--;
        this.pump();
      }
    })();
    entry.p.catch(() => {
      // A newer entry for the same URL (asked again after this one was cancelled) stays.
      if (this.m.get(url) === entry) this.m.delete(url);
    });
    this.m.set(url, entry);
    this.evict();
    return entry.p;
  }

  /** Resolves once the entry may run (at once when a slot is free). */
  private slot(e: Entry): Promise<void> {
    if (this.running < this.slots) {
      this.running++;
      return Promise.resolve();
    }
    return new Promise((resolve, reject) => {
      e.start = () => {
        this.running++;
        resolve();
      };
      e.ctl.signal.addEventListener('abort', () => reject(aborted()), { once: true });
    });
  }

  private pump() {
    while (this.running < this.slots) {
      let next: Entry | undefined;
      for (const e of this.m.values()) {
        if (e.start && (!next || (next.low && !e.low) || (next.low === e.low && e.seq < next.seq))) next = e;
      }
      if (!next) return;
      const start = next.start!;
      next.start = undefined;
      start();
    }
  }

  private async load(url: string, entry: Entry): Promise<Decoded> {
    try {
      return await this.loadTimed(url, entry);
    } catch (e) {
      if (tel.on) tel.count(`img.${(e as Error).name === 'AbortError' ? 'aborted' : 'failed'}.${kindOf(url)}`);
      throw e;
    }
  }

  private async loadTimed(url: string, entry: Entry): Promise<Decoded> {
    const { signal } = entry.ctl;
    const t0 = performance.now();
    const res = await fetch(url, { signal, priority: entry.low ? 'low' : 'high', credentials: 'same-origin' } as RequestInit);
    if (!res.ok) {
      let msg = res.statusText;
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        /* ignore */
      }
      throw new Error(msg);
    }
    const blob = await res.blob();
    const t1 = performance.now();
    let bmp = await createImageBitmap(blob);
    if (tel.on) {
      // Debug mode: getting the bytes, then decoding them (off the main thread).
      const k = kindOf(url);
      tel.time(`img.fetch.${k}${entry.low ? '.ahead' : ''}`, t1 - t0);
      tel.time(`img.decode.${k}`, performance.now() - t1);
    }
    if (bmp.width > this.maxTex || bmp.height > this.maxTex) {
      const s = this.maxTex / Math.max(bmp.width, bmp.height);
      const small = await createImageBitmap(bmp, { resizeWidth: Math.floor(bmp.width * s), resizeHeight: Math.floor(bmp.height * s), resizeQuality: 'high' });
      bmp.close();
      bmp = small;
    }
    // Cancelled while decoding (which cannot be stopped): nobody holds the bitmap.
    if (signal.aborted) {
      bmp.close();
      throw aborted();
    }
    const d = { bmp, w: bmp.width, h: bmp.height, url };
    entry.done = d;
    return d;
  }

  /** Cancels the loads not in `keep`: waiting ones, and running ones too unless
   * `running` is false. */
  cancelOthers(keep: Set<string>, running = true) {
    for (const [url, e] of this.m) {
      if (e.done || keep.has(url) || (!running && !e.start)) continue;
      e.ctl.abort();
      this.m.delete(url);
    }
  }

  private evict() {
    if (this.m.size <= this.max) return;
    const sorted = [...this.m.entries()].sort((a, b) => a[1].t - b[1].t);
    for (const [url, e] of sorted) {
      if (this.m.size <= this.max) break;
      if (!e.done) continue;
      e.done.bmp.close();
      this.m.delete(url);
    }
  }

  clear() {
    for (const e of this.m.values()) {
      e.ctl.abort();
      e.done?.bmp.close();
    }
    this.m.clear();
  }
}
