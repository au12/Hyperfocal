// Grid thumbnails: loaded a few at a time, nearest the viewport first, and only for
// tiles that are still wanted when their turn comes.
//
// With plain <img src> tags the browser fetches in the order tiles appeared, six at a
// time at most over HTTP/1.1. Scroll a long way and every tile passed on the way joins
// the queue ahead of the ones now on screen (and the server makes thumbnails nobody
// will look at). Here the grid ranks what is waiting each time a load finishes, tiles
// that scroll away drop out before their turn, and a load that is taking a while (the
// server making the thumbnail) is cancelled when nothing wants it any more.
//
// Loads go through ordinary images (cheaper in the browser than fetch and blobs); the
// tile's own <img> gets the URL once it is in the browser's cache.

interface Want {
  url: string;
  /** Position in the photo list: the grid ranks by it. */
  i: number;
  /** Tiles waiting for it. None: wanted only ahead of scrolling. */
  tiles: Set<HTMLImageElement>;
  ahead: boolean;
}

interface Load {
  img: HTMLImageElement;
  t0: number;
}

/** A load running this long is probably a thumbnail being made: cancel it when it is no
 *  longer wanted. A younger one is left to finish (it is nearly done, and cancelling
 *  costs an HTTP/1.1 connection). */
const CANCEL_AFTER_MS = 250;
/** A failed thumbnail (unreadable file, server away) is retried after this long. */
const RETRY_MS = 30_000;
/** URLs known to be in the browser's cache (shown at once, no turn needed). */
const KNOWN = 5000;

function protocol(url?: string): string {
  try {
    const e = url
      ? (performance.getEntriesByName(new URL(url, location.href).href)[0] as PerformanceResourceTiming | undefined)
      : (performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined);
    return e?.nextHopProtocol ?? '';
  } catch {
    return '';
  }
}
const multiplexed = (p: string) => p === 'h2' || p.startsWith('h3');

export class ThumbLoader {
  private wants = new Map<string, Want>();
  private loads = new Map<string, Load>();
  private known = new Set<string>();
  private failed = new Map<string, number>();
  private held = false;
  private holdTimer: ReturnType<typeof setTimeout> | undefined;
  private scheduled = false;
  private probed = false;
  /** HTTP/2 and 3 share one connection: more at once. HTTP/1.1 has six per host, one of
   *  them the live events stream. */
  private max = multiplexed(protocol()) ? 10 : 5;

  /** Lower is sooner. Set by the grid (distance from the viewport). */
  rank: (i: number) => number = (i) => i;

  /** A tile's <img> wants `url`. Returns the release. */
  want(img: HTMLImageElement, url: string, i: number): () => void {
    if (this.known.has(url)) {
      img.src = url;
      return () => {};
    }
    let w = this.wants.get(url);
    if (!w) {
      w = { url, i, tiles: new Set(), ahead: false };
      this.wants.set(url, w);
    }
    w.i = i;
    w.tiles.add(img);
    this.schedule();
    const held = w;
    return () => {
      if (held.tiles.delete(img)) this.drop(held);
    };
  }

  /** The tile showing `url` moved in the list (a sort or filter). */
  move(url: string, i: number) {
    const w = this.wants.get(url);
    if (w) w.i = i;
  }

  /** Thumbnails to load ahead of scrolling, after everything on screen. Replaces the
   *  previous set. */
  ahead(list: { url: string; i: number }[]) {
    const keep = new Set<string>();
    for (const { url, i } of list) {
      if (this.known.has(url) || this.recentlyFailed(url)) continue;
      keep.add(url);
      let w = this.wants.get(url);
      if (!w) {
        w = { url, i, tiles: new Set(), ahead: true };
        this.wants.set(url, w);
      }
      w.ahead = true;
      w.i = i;
    }
    for (const w of [...this.wants.values()]) {
      if (w.ahead && !keep.has(w.url)) {
        w.ahead = false;
        this.drop(w);
      }
    }
    this.schedule();
  }

  /** Start nothing new for `ms` (scrolling very fast: the tiles flying past are not
   *  worth loading). Loads already running finish. */
  hold(ms: number) {
    this.held = true;
    clearTimeout(this.holdTimer);
    this.holdTimer = setTimeout(() => {
      this.held = false;
      this.schedule();
    }, ms);
  }

  private recentlyFailed(url: string) {
    const t = this.failed.get(url);
    return t != null && performance.now() - t < RETRY_MS;
  }

  /** No tile wants `w` any more (and it is not wanted ahead). */
  private drop(w: Want) {
    if (w.tiles.size || w.ahead) return;
    this.wants.delete(w.url);
    const l = this.loads.get(w.url);
    if (l && performance.now() - l.t0 > CANCEL_AFTER_MS) {
      this.loads.delete(w.url);
      l.img.onload = l.img.onerror = null;
      l.img.removeAttribute('src'); // cancels the request
      this.schedule();
    }
  }

  private schedule() {
    if (this.scheduled) return;
    this.scheduled = true;
    // After every tile rendered this frame has said what it wants.
    queueMicrotask(() => {
      this.scheduled = false;
      this.pump();
    });
  }

  private pump() {
    if (this.held) return;
    const free = this.max - this.loads.size;
    if (free <= 0) return;
    const waiting: { w: Want; r: number }[] = [];
    for (const w of this.wants.values()) {
      if (this.loads.has(w.url) || this.recentlyFailed(w.url)) continue;
      waiting.push({ w, r: this.rank(w.i) });
    }
    if (!waiting.length) return;
    waiting.sort((a, b) => a.r - b.r || a.w.i - b.w.i);
    for (const { w, r } of waiting.slice(0, free)) this.start(w, r < 1);
  }

  private start(w: Want, onScreen: boolean) {
    const url = w.url;
    const img = new Image();
    const load: Load = { img, t0: performance.now() };
    this.loads.set(url, load);
    const done = (ok: boolean) => {
      img.onload = img.onerror = null;
      if (this.loads.get(url) !== load) return;
      this.loads.delete(url);
      const now = this.wants.get(url);
      this.wants.delete(url);
      if (ok) {
        this.known.add(url);
        if (this.known.size > KNOWN) this.known.delete(this.known.values().next().value!);
        // From the browser's cache now.
        if (now) for (const t of now.tiles) t.src = url;
      } else this.failed.set(url, performance.now());
      if (!this.probed && ok) this.probe(url);
      this.schedule();
    };
    img.onload = () => done(true);
    img.onerror = () => done(false);
    img.decoding = 'async';
    (img as HTMLImageElement & { fetchPriority: string }).fetchPriority = onScreen ? 'high' : 'low';
    img.src = url;
  }

  /** The page came from the service worker (which hides the protocol): the server's own
   *  answer for a thumbnail says. */
  private probe(url: string) {
    this.probed = true;
    if (multiplexed(protocol(url))) this.max = 10;
  }
}

export const thumbs = new ThumbLoader();

/** `use:thumb={{ url, i }}` on an <img>: its src is set when the loader gets to it. */
export function thumb(node: HTMLImageElement, p: { url: string; i: number }) {
  let cur = p;
  let release = thumbs.want(node, p.url, p.i);
  return {
    update(n: { url: string; i: number }) {
      if (n.url !== cur.url) {
        // A new version (a draft replaced): the old image stays until the new one is set.
        release();
        release = thumbs.want(node, n.url, n.i);
      } else if (n.i !== cur.i) thumbs.move(n.url, n.i);
      cur = n;
    },
    destroy() {
      release();
    },
  };
}
