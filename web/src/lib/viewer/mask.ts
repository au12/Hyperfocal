// The RAW clipping mask of the photo on screen, from the server (PNG; pixel = MASK_* bits).
//
// The overexposure display follows the exposure correction, as FastRawViewer's does, so a
// new correction asks for a new mask. While it loads, the previous mask of the same photo
// stays up: stepping the EV never blanks the overlay, and a quick run of steps asks only
// for the last one. Another photo or white balance drops the old mask at once, so one
// photo's mask is never drawn over another.

export interface MaskTarget {
  setMask(bmp: ImageBitmap | null): void;
}

/** How long a new correction waits for the next step before its mask is fetched. */
const SETTLE_MS = 140;

export class MaskLoader {
  /** A mask is shown or on its way for the current photo: the view then draws no
   * overlay from the shown image (a misleading guess) while it waits. */
  active = false;
  private base = '';
  private url = '';
  private shownForBase = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private ctl: AbortController | null = null;

  constructor(
    private target: () => MaskTarget | null,
    private redraw: () => void,
  ) {}

  /**
   * `base`: what the mask belongs to apart from the correction (photo, version, white
   * balance), '' for no RAW mask at all. `url`: the mask wanted now. `want`: whether any
   * overlay is on; when none is, the mask shown is kept (toggling back is instant) but
   * nothing new is fetched.
   */
  update(base: string, url: string, want: boolean) {
    const t = this.target();
    if (!t) return;
    if (base !== this.base) {
      this.cancel();
      if (this.url || this.shownForBase) {
        t.setMask(null);
        this.redraw();
      }
      this.base = base;
      this.url = '';
      this.shownForBase = false;
      this.active = false;
    }
    if (!base || !url || url === this.url || !want) return;
    this.url = url;
    this.active = true;
    this.cancel();
    if (this.shownForBase) this.timer = setTimeout(() => this.fetch(url), SETTLE_MS);
    else this.fetch(url);
  }

  private cancel() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.ctl?.abort();
    this.ctl = null;
  }

  private async fetch(url: string) {
    this.timer = null;
    const ctl = new AbortController();
    this.ctl = ctl;
    try {
      const res = await fetch(url, { signal: ctl.signal, credentials: 'same-origin' });
      if (!res.ok) throw new Error(res.statusText);
      const bmp = await createImageBitmap(await res.blob(), { colorSpaceConversion: 'none', premultiplyAlpha: 'none' });
      const t = this.target();
      if (!t || this.url !== url || ctl.signal.aborted) return bmp.close();
      t.setMask(bmp);
      bmp.close();
      this.shownForBase = true;
      this.redraw();
    } catch (e) {
      if ((e as Error).name === 'AbortError') return;
      // No RAW mask (e.g. the RAW failed to develop): the view falls back to the image.
      const t = this.target();
      if (t && this.url === url) {
        this.url = '';
        this.active = false;
        this.shownForBase = false;
        t.setMask(null);
        this.redraw();
      }
    }
  }

  destroy() {
    this.cancel();
  }
}
