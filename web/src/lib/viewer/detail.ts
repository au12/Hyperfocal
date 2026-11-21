// Detail tiles drawn over the base image (gl.ts) when zoomed in:
// - RAW view: at about 1:1 or closer, full-resolution tiles demosaiced with RCD, either
//   on the server (JPEG tiles) or on this device's GPU (rcd.ts, from the tile's sensor
//   samples), over the half-size image.
// - Everything else (JPEG, HEIC, TIFF, PNG, a RAW file's embedded JPEG): tiles of a zoom
//   pyramid (full size and its halvings), at the level that fits the zoom, over the
//   screen-size preview, so a 45-100 MP image never loads whole.
// Tiles nearest the centre of the view come first; ones that scroll away are cancelled;
// the least recently seen are dropped when the tile memory is full.

import type { GLView } from './gl';
import { RcdGpu, parseCfaTile, type CfaTile } from './rcd';
import { urls, type Pyramid, type ToneMode, type WbMode } from '../api';

/** What is shown: the photo, its development and the full-resolution geometry. */
export interface DetailSpec {
  id: number;
  v: string;
  mode: ToneMode;
  ev: number;
  wb: WbMode;
  /** Full-resolution frame size (sensor orientation), tile size and apron. */
  full: [number, number];
  tile: number;
  apron: number;
  /** Server version (for caching the tone curve). */
  version: string;
  /** 'image': zoom tiles of the non-RAW view at pyramid `level` (`full` is that level's
   * size); else full-resolution RAW tiles. */
  kind?: 'raw' | 'image';
  level?: number;
}

/** The view, as passed to GLView.draw. */
export interface DetailView {
  orientation: number;
  dispW: number;
  dispH: number;
  scale: number;
  cx: number;
  cy: number;
}

export type DetailPath = 'gpu' | 'server';

/** Device px per full-resolution px from which tiles are shown. */
export const DETAIL_FROM = 0.75;

interface Slot {
  tx: number;
  ty: number;
  /** Tone the layer was rendered with (server tiles: mode and EV; GPU: also). */
  tone: string;
  used: number;
  /** The samples, kept to re-render on a tone change without fetching (GPU path). */
  cfa?: CfaTile;
}

const MAX_INFLIGHT = { gpu: 6, server: 4 };

/** The server could not make a tile (not a failure of this device). */
class TileUnavailable extends Error {}

export class DetailLayer {
  private spec: DetailSpec | null = null;
  /** Identity of tile contents apart from the tone: photo, version, white balance. */
  private base = '';
  private slots: (Slot | null)[] = [];
  private at = new Map<string, number>();
  private inflight = new Map<string, { ctl: AbortController; tone: string }>();
  private tick = 0;
  private rcd: RcdGpu | null = null;
  private lut: { key: string; p: Promise<void> } | null = null;
  private visible = new Set<string>();
  private queue: { key: string; tx: number; ty: number; d: number }[] = [];
  private layers = 0;
  private geoKey = '';
  /** Whether tiles are being drawn now. */
  active = false;
  /** Set when something failed for good (e.g. the GPU path), for diagnostics. */
  error = '';

  constructor(
    private gl: GLView,
    public path: DetailPath,
    /** Called when a tile arrives, to redraw. */
    private redraw: () => void,
  ) {
    if (path === 'gpu') {
      try {
        this.rcd = new RcdGpu(gl.gl);
      } catch (e) {
        this.error = (e as Error).message;
        this.path = 'server';
      }
    }
  }

  /** Whether this device can demosaic on its GPU. */
  static gpuSupported(gl: WebGL2RenderingContext): boolean {
    return RcdGpu.supported(gl);
  }

  private tone(s: DetailSpec) {
    // Image tiles are plain display pixels: the exposure correction is applied on screen.
    return s.kind === 'image' ? 'image' : `${s.mode}|${s.ev.toFixed(2)}`;
  }

  /** How tiles of the current spec are made: image tiles always come from the server. */
  private via(): DetailPath {
    return this.spec?.kind === 'image' ? 'server' : this.path;
  }

  /** Sets what is shown (null: no detail for the current photo). */
  setSpec(s: DetailSpec | null) {
    const base = s ? `${s.kind ?? 'raw'}|${s.level ?? 0}|${s.id}|${s.v}|${s.kind === 'image' ? '' : s.wb}|${s.full[0]}x${s.full[1]}|${s.tile}|${s.apron}` : '';
    if (base !== this.base) {
      this.reset();
      this.base = base;
    }
    this.spec = s;
    if (s && s.kind !== 'image' && this.path === 'gpu') this.loadLut(s);
  }

  private reset() {
    for (const f of this.inflight.values()) f.ctl.abort();
    this.inflight.clear();
    this.queue = [];
    this.slots = this.slots.map(() => null);
    this.at.clear();
    this.gl.detailClear();
    this.gl.detailEnable(false);
    this.active = false;
  }

  /** The tone curve for the GPU path; re-renders the tiles held once it is in. */
  private loadLut(s: DetailSpec) {
    const key = this.tone(s);
    if (this.lut?.key === key) return;
    const p = (async () => {
      const res = await fetch(urls.lut(s.mode, s.ev, s.version), { credentials: 'same-origin' });
      if (!res.ok) throw new Error(`tone curve: ${res.status}`);
      const lut = new Uint8Array(await res.arrayBuffer());
      if (this.lut?.key !== key || !this.rcd) return;
      this.rcd.setLut(lut, key);
      this.rerender();
    })().catch((e) => {
      if (this.lut?.key === key) this.lut = null;
      // Offline or the like: tiles wait for the next attempt.
      if (!(e instanceof TypeError)) this.fail(e);
    });
    this.lut = { key, p };
  }

  /** GPU path: tiles held with an older tone, rendered again from their samples. */
  private rerender() {
    if (!this.rcd || !this.spec) return;
    const tone = this.tone(this.spec);
    let any = false;
    this.slots.forEach((sl, layer) => {
      if (!sl?.cfa || sl.tone === tone) return;
      const target = this.gl.detailTarget(layer);
      if (!target) return;
      this.rcd!.render(sl.cfa, target);
      sl.tone = tone;
      any = true;
    });
    if (any) this.redraw();
  }

  private fail(e: unknown) {
    if ((e as Error)?.name === 'AbortError' || e instanceof TileUnavailable || e instanceof TypeError) return;
    // The GPU path failed for good (no float targets, a shader error): use server tiles.
    if (this.path === 'gpu') {
      this.error = (e as Error)?.message ?? String(e);
      console.warn('GPU demosaicing off:', this.error);
      this.path = 'server';
      this.rcd?.destroy();
      this.rcd = null;
      const s = this.spec;
      this.base = '';
      this.setSpec(s);
      this.redraw();
    }
  }

  /** Tiles for the view; call on every draw (cheap when nothing changes). Returns whether
   * tiles are drawn. */
  update(v: DetailView, canvasW: number, canvasH: number): boolean {
    const s = this.spec;
    const gl = this.gl;
    // Device px per full-resolution px.
    const fullDispW = s ? (v.orientation >= 5 ? s.full[1] : s.full[0]) : 0;
    const fs = s && fullDispW > 0 ? (v.scale * v.dispW) / fullDispW : 0;
    if (!s || fs < DETAIL_FROM) return this.off();
    // Tile memory: what the canvas can show at the threshold, plus slack. Grown when the
    // canvas grows (dropping the tiles held), never shrunk.
    const geo = `${s.tile}|${s.apron}|${s.full[0]}x${s.full[1]}`;
    const across = Math.ceil(canvasW / (DETAIL_FROM * s.tile)) + 1;
    const down = Math.ceil(canvasH / (DETAIL_FROM * s.tile)) + 1;
    const coarse = matchMedia('(pointer: coarse)').matches;
    const grid = Math.ceil(s.full[0] / s.tile) * Math.ceil(s.full[1] / s.tile);
    const need = Math.min(grid, across * down + 8, coarse ? 48 : 96);
    if (geo !== this.geoKey || need > this.layers || !gl.detail) {
      for (const f of this.inflight.values()) f.ctl.abort();
      this.inflight.clear();
      gl.detailConfigure(s.full, s.tile, s.apron, Math.max(need, geo === this.geoKey ? this.layers : 0));
      this.layers = gl.detail?.layers ?? need;
      this.slots = new Array(this.layers).fill(null);
      this.at.clear();
      this.geoKey = geo;
    }
    // The part of the frame in view, in full-resolution px (sensor orientation).
    const corners = [
      [0, 0],
      [canvasW, 0],
      [0, canvasH],
      [canvasW, canvasH],
    ].map(([x, y]) => {
      const ix = v.cx + (x - canvasW / 2) / v.scale;
      const iy = v.cy + (y - canvasH / 2) / v.scale;
      const [u, w] = orient(ix / v.dispW, iy / v.dispH, v.orientation);
      return [u * s.full[0], w * s.full[1]];
    });
    const x0 = Math.max(0, Math.min(...corners.map((c) => c[0])));
    const x1 = Math.min(s.full[0], Math.max(...corners.map((c) => c[0])));
    const y0 = Math.max(0, Math.min(...corners.map((c) => c[1])));
    const y1 = Math.min(s.full[1], Math.max(...corners.map((c) => c[1])));
    const t = s.tile;
    const want: { key: string; tx: number; ty: number; d: number }[] = [];
    const mx = (x0 + x1) / 2;
    const my = (y0 + y1) / 2;
    for (let ty = Math.floor(y0 / t); ty * t < y1; ty++) {
      for (let tx = Math.floor(x0 / t); tx * t < x1; tx++) {
        want.push({ key: `${tx},${ty}`, tx, ty, d: Math.hypot((tx + 0.5) * t - mx, (ty + 0.5) * t - my) });
      }
    }
    // More tiles in view than memory: show the half-size image until zoomed in further.
    if (want.length > this.layers) return this.off();
    want.sort((a, b) => a.d - b.d);
    this.visible = new Set(want.map((w) => w.key));
    const tick = ++this.tick;
    for (const w of want) {
      const layer = this.at.get(w.key);
      if (layer != null) this.slots[layer]!.used = tick;
    }
    // Requests no longer in view are cancelled.
    for (const [key, f] of this.inflight) {
      if (!this.visible.has(key)) {
        f.ctl.abort();
        this.inflight.delete(key);
      }
    }
    const tone = this.tone(s);
    this.queue = want.filter((w) => {
      const layer = this.at.get(w.key);
      if (layer == null) return true;
      // Held but rendered with another tone: server tiles are fetched again (GPU ones
      // are re-rendered when the tone curve arrives).
      return this.via() === 'server' && this.slots[layer]!.tone !== tone;
    });
    this.pump();
    this.active = true;
    gl.detailEnable(true);
    return true;
  }

  private off(): boolean {
    if (this.active) {
      this.active = false;
      this.gl.detailEnable(false);
    }
    this.queue = [];
    for (const f of this.inflight.values()) f.ctl.abort();
    this.inflight.clear();
    return false;
  }

  private pump() {
    const s = this.spec;
    if (!s) return;
    const tone = this.tone(s);
    const via = this.via();
    while (this.queue.length && this.inflight.size < MAX_INFLIGHT[via]) {
      const w = this.queue.shift()!;
      const f = this.inflight.get(w.key);
      if (f && f.tone === tone) continue;
      // Arrived since the queue was made.
      const held = this.at.get(w.key);
      if (held != null && (via === 'gpu' || this.slots[held]?.tone === tone)) continue;
      f?.ctl.abort();
      const ctl = new AbortController();
      this.inflight.set(w.key, { ctl, tone });
      this.fetchTile(s, w.tx, w.ty, w.key, tone, ctl).finally(() => {
        if (this.inflight.get(w.key)?.ctl === ctl) this.inflight.delete(w.key);
        this.pump();
      });
    }
  }

  private async fetchTile(s: DetailSpec, tx: number, ty: number, key: string, tone: string, ctl: AbortController) {
    const base = this.base;
    try {
      if (s.kind !== 'image' && this.path === 'gpu' && this.rcd) {
        const res = await fetch(urls.cfatile(s.id, s.v, s.wb, tx, ty), { signal: ctl.signal, credentials: 'same-origin' });
        if (!res.ok) throw new TileUnavailable(`tile ${key}: ${res.status}`);
        const cfa = parseCfaTile(await res.arrayBuffer());
        await this.lut?.p;
        if (ctl.signal.aborted || base !== this.base || !this.rcd || !this.spec || !this.rcd.lutKey) return;
        const layer = this.place(tx, ty, key);
        const target = this.gl.detailTarget(layer);
        if (!target) return;
        this.rcd.render(cfa, target);
        this.slots[layer] = { tx, ty, tone: this.rcd.lutKey, used: this.tick, cfa };
        this.gl.detailSetPage(tx, ty, layer);
      } else {
        const url = s.kind === 'image' ? urls.ptile(s.id, s.v, s.level ?? 0, tx, ty) : urls.rawtile(s.id, s.v, { mode: s.mode, ev: s.ev, wb: s.wb, tx, ty });
        const res = await fetch(url, { signal: ctl.signal, credentials: 'same-origin' });
        if (!res.ok) throw new TileUnavailable(`tile ${key}: ${res.status}`);
        const bmp = await createImageBitmap(await res.blob());
        if (ctl.signal.aborted || base !== this.base) return bmp.close();
        const layer = this.place(tx, ty, key);
        this.gl.detailUpload(layer, bmp);
        bmp.close();
        this.slots[layer] = { tx, ty, tone, used: this.tick };
        this.gl.detailSetPage(tx, ty, layer);
      }
      this.redraw();
    } catch (e) {
      // A tile that cannot be made (e.g. a RAW whose full resolution failed) stays on
      // the half-size image; the view keeps working. A GPU failure switches to server
      // tiles.
      this.fail(e);
    }
  }

  /** A layer for tile (tx, ty): its own, a free one, or the least recently seen one
   * out of view. */
  private place(tx: number, ty: number, key: string): number {
    const own = this.at.get(key);
    if (own != null) return own;
    let pick = this.slots.indexOf(null);
    if (pick < 0) {
      let oldest = Infinity;
      this.slots.forEach((sl, i) => {
        if (sl && !this.visible.has(`${sl.tx},${sl.ty}`) && sl.used < oldest) {
          oldest = sl.used;
          pick = i;
        }
      });
      if (pick < 0) pick = 0;
      const old = this.slots[pick]!;
      this.gl.detailSetPage(old.tx, old.ty, -1);
      this.at.delete(`${old.tx},${old.ty}`);
    }
    this.at.set(key, pick);
    this.slots[pick] = { tx, ty, tone: '', used: this.tick };
    return pick;
  }

  destroy() {
    this.reset();
    this.rcd?.destroy();
    this.rcd = null;
    this.gl.detailRelease();
  }
}

/** The largest preview the server makes (long edge): the non-RAW view's base image. */
export const PREVIEW_MAX = 4096;

/**
 * Zoom tiles of the non-RAW view for a zoom: the pyramid level that fits it (the coarsest
 * still at least DETAIL_FROM device px per pixel), once the view needs more than the
 * largest preview. Null when the preview is as sharp (or when zoomed to fit).
 */
export function imageDetailSpec(
  p: Pyramid | null,
  photo: { id: number; v: string; version: string },
  view: { dispW: number; dispH: number; scale: number; orientation: number; fit: boolean },
): DetailSpec | null {
  if (!p || p.levels === 0 || view.fit) return null;
  const base = Math.min(PREVIEW_MAX, Math.max(p.w, p.h));
  if (Math.max(view.dispW, view.dispH) * view.scale <= base * 1.02) return null;
  // Device px per full-size px.
  const fs0 = (view.scale * view.dispW) / (view.orientation >= 5 ? p.h : p.w);
  const level = Math.min(p.levels - 1, Math.max(0, Math.ceil(Math.log2(DETAIL_FROM / fs0) - 1e-9)));
  const d = 2 ** level;
  const full: [number, number] = [Math.ceil(p.w / d), Math.ceil(p.h / d)];
  if (Math.max(full[0], full[1]) <= base) return null;
  return { ...photo, kind: 'image', level, full, tile: p.tile, apron: p.apron, mode: 'normal', ev: 0, wb: 'asshot' };
}

/** EXIF orientation: displayed (0..1) to sensor (0..1) coordinates, as the shader's orient(). */
function orient(x: number, y: number, o: number): [number, number] {
  switch (o) {
    case 2:
      return [1 - x, y];
    case 3:
      return [1 - x, 1 - y];
    case 4:
      return [x, 1 - y];
    case 5:
      return [y, x];
    case 6:
      return [y, 1 - x];
    case 7:
      return [1 - y, 1 - x];
    case 8:
      return [1 - y, x];
    default:
      return [x, y];
  }
}
