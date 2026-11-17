// Decoded-image cache for the single view: fetches with priorities, decodes off the
// main thread (createImageBitmap) and keeps the most recent few bitmaps.

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
}

export class ImageCache {
  private m = new Map<string, Entry>();
  private tick = 0;
  constructor(private max = 12, private maxTex = 16384) {}

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
      if (!low && hit.low && !hit.done) hit.low = false;
      return hit.p;
    }
    const ctl = new AbortController();
    const entry: Entry = { ctl, low, t: ++this.tick } as Entry;
    entry.p = (async () => {
      const res = await fetch(url, { signal: ctl.signal, priority: low ? 'low' : 'high', credentials: 'same-origin' } as RequestInit);
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
      let bmp = await createImageBitmap(blob);
      if (bmp.width > this.maxTex || bmp.height > this.maxTex) {
        const s = this.maxTex / Math.max(bmp.width, bmp.height);
        const small = await createImageBitmap(bmp, { resizeWidth: Math.floor(bmp.width * s), resizeHeight: Math.floor(bmp.height * s), resizeQuality: 'high' });
        bmp.close();
        bmp = small;
      }
      const d = { bmp, w: bmp.width, h: bmp.height, url };
      entry.done = d;
      return d;
    })();
    entry.p.catch(() => this.m.delete(url));
    this.m.set(url, entry);
    this.evict();
    return entry.p;
  }

  /** Cancel low-priority requests not in `keep`. */
  cancelOthers(keep: Set<string>) {
    for (const [url, e] of this.m) {
      if (!e.done && e.low && !keep.has(url)) {
        e.ctl.abort();
        this.m.delete(url);
      }
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
