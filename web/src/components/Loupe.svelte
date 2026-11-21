<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { store, LABELS, fmtSettings, cameraName, SHARPEN_SIGMA } from '../lib/store.svelte';
  import { api, urls, type Pyramid, type RawInfo } from '../lib/api';
  import { GLView, hexToRgb } from '../lib/viewer/gl';
  import { ImageCache, type Decoded } from '../lib/viewer/loader';
  import { DetailLayer, imageDetailSpec, type DetailPath } from '../lib/viewer/detail';
  import { MaskLoader } from '../lib/viewer/mask';
  import { viewerCtl, toggleFullscreen, ZOOM_STEPS } from '../lib/ctl';
  import Icon from './Icon.svelte';
  import Histogram from './Histogram.svelte';
  import InfoPanel from './InfoPanel.svelte';
  import FaceOverlay from './FaceOverlay.svelte';
  import { people } from '../lib/people.svelte';
  import { flip } from 'svelte/animate';
  import { flyImage, segmented, bump, reduced, type Rect } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';

  let stage: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let gl: GLView | null = null;
  let detail: DetailLayer | null = null;
  /** Full-resolution tiles are being drawn, and where they are demosaiced (zoom badge). */
  let detailOn = $state(false);
  let detailPath = $state<DetailPath | null>(null);
  let glError = $state('');
  const cache = new ImageCache(16);

  // View transform (device px per image px; centre in displayed image px)
  let fit = $state(true);
  let scale = $state(1);
  let cx = 0;
  let cy = 0;
  let cw = $state(1);
  let ch = $state(1);

  let shownUrl = $state('');
  let shownKind = $state<'thumb' | 'preview' | 'full' | 'raw'>('thumb');
  let shownId = -1;
  let loadError = $state('');
  const masks = new MaskLoader(() => gl, () => requestDraw());
  let rawInfo = $state<RawInfo | null>(null);
  /** The zoom pyramid of the current photo's non-RAW view: 'loading' until known, null
   * when there is none (zooming in then loads the full-size image whole). */
  let pyr = $state<{ id: number; v: string; p: Pyramid | null } | 'loading' | null>(null);
  /** The pyramid level the zoom tiles of the non-RAW view come from (-1: none). */
  let imgLevel = $state(-1);
  let jpegHist = $state<Uint32Array[] | null>(null);
  let chrome = $state(true);
  /** Where the image is on screen (CSS px in the stage), for the face overlay. */
  let imgRect = $state<{ x: number; y: number; w: number; h: number } | null>(null);
  let sheet = $state<null | 'info' | 'tools' | 'rate'>(null);
  let currentBmp: Decoded | null = null;

  const L = $derived(store.list);
  const i = $derived(store.current);
  const id = $derived(L.id[i]);
  const v = $derived(L.v[i]);
  const o = $derived(L.o[i] ?? 1);
  const isRaw = $derived(L.k[i] === 1);
  /** Name of the non-RAW source shown: the RAW's embedded JPEG, or the file's own format. */
  const fileLabel = $derived(({ 3: 'TIFF', 4: 'PNG', 5: 'HEIC' } as Record<number, string>)[L.k[i]] ?? 'JPEG');
  const dispW = $derived((o >= 5 ? L.h[i] : L.w[i]) || 3);
  const dispH = $derived((o >= 5 ? L.w[i] : L.h[i]) || 2);
  const useRaw = $derived(isRaw && store.viewer.source === 'raw');
  // Filmstrip: enough slots either side to fill the strip, current photo in the middle.
  let filmW = $state(0);
  const remPx = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  const filmHalf = $derived(Math.max(4, Math.ceil(filmW / (6.25 * remPx) / 2) + 1));
  /** The photo indexes the filmstrip shows (out-of-range ones are spacers), keyed by
   * photo so the strip slides as you move. */
  const filmSlots = $derived(Array.from({ length: filmHalf * 2 + 1 }, (_, k) => i - filmHalf + k));
  const ev = $derived(store.viewer.evOn ? store.viewer.ev : 0);
  const fitScale = $derived(Math.min(cw / dispW, ch / dispH));
  const curScale = $derived(fit ? fitScale : scale);
  const zoomPct = $derived(Math.round(curScale * 100));
  const inReject = $derived(!!store.folders.get(L.f[i])?.isReject);
  // Rating, label and pick change in place in the list (optimistic edits); `rev` is what
  // tells the view, so these must read it or they wait for the server's list reload.
  const cull = $derived.by(() => {
    void store.rev;
    return { r: L.r[i] ?? 0, lc: L.lc[i] ?? null, p: L.p[i] ?? 0 };
  });

  const SIZES = [1024, 1600, 2048, 2560, 3200, 4096];

  function wanted(idx: number, forScale: number): { url: string; kind: 'preview' | 'full' | 'raw' } | null {
    const pid = L.id[idx];
    if (pid == null) return null;
    const oo = L.o[idx] ?? 1;
    const w = (oo >= 5 ? L.h[idx] : L.w[idx]) || 3;
    const h = (oo >= 5 ? L.w[idx] : L.h[idx]) || 2;
    const sc = forScale > 0 ? forScale : Math.min(cw / w, ch / h);
    const need = Math.max(w, h) * sc;
    const raw = L.k[idx] === 1 && store.viewer.source === 'raw';
    if (raw) {
      const size = need > 4096 * 1.05 ? 0 : (SIZES.find((s) => s >= need) ?? 0);
      return { url: urls.raw(pid, L.v[idx], { w: size, mode: store.viewer.tone, ev, wb: store.viewer.wb }), kind: 'raw' };
    }
    // The photo on screen, zoomed in: zoom tiles show the detail (syncDetail), so the base
    // stays a preview of at most 4096 px. Without tiles, the full-size image loads whole.
    if (idx === i && forScale > 0 && tilesUsable()) return { url: urls.preview(pid, L.v[idx], SIZES.find((s) => s >= need) ?? 4096), kind: 'preview' };
    if (need > 2600 || forScale > 0) return { url: urls.jpeg(pid, L.v[idx]), kind: 'full' };
    return { url: urls.preview(pid, L.v[idx], SIZES.find((s) => s >= need) ?? 2048), kind: 'preview' };
  }

  /** Zoom tiles can stand in for the full-size image of the photo on screen (or will,
   * once its pyramid is known). */
  function tilesUsable(): boolean {
    if (!detail || useRaw || id == null) return false;
    if (pyr === 'loading') return true;
    return pyr != null && pyr.id === id && pyr.v === v && pyr.p != null && pyr.p.levels > 0;
  }

  async function loadPyramid() {
    if (id == null || useRaw || !detail) {
      pyr = null;
      return;
    }
    const pid = id;
    const pv = v;
    pyr = 'loading';
    try {
      const p = await api.pyramid(pid, pv);
      if (id === pid) pyr = { id: pid, v: pv, p };
    } catch {
      if (id === pid) pyr = { id: pid, v: pv, p: null };
    }
  }

  function show(d: Decoded, kind: typeof shownKind) {
    if (!gl) return;
    currentBmp = d;
    gl.setImage(d.bmp, d.w, d.h);
    shownUrl = d.url;
    shownKind = kind;
    loadError = '';
    if (store.panels.histogram && !useRaw) computeJpegHist(d);
    requestDraw();
  }

  async function load() {
    if (!gl || id == null) return;
    const pid = id;
    const want = wanted(i, fit ? 0 : curScale);
    if (!want) return;
    if (shownUrl === want.url) return;
    const hit = cache.peek(want.url);
    if (hit) {
      show(hit, want.kind);
    } else {
      // Something to look at right away: the thumbnail (already in the browser cache).
      if (shownId !== pid) {
        const thumbUrl = urls.thumb(pid, v);
        const t = cache.peek(thumbUrl);
        // Thumbnails are stored upright; the viewer expects sensor orientation, so
        // only use them for unrotated photos.
        if (o === 1) {
          if (t) show(t, 'thumb');
          else cache.get(thumbUrl).then((d) => { if (id === pid && shownId !== pid) show(d, 'thumb'); }).catch(() => {});
        } else if (gl) {
          gl.setImage(null);
          requestDraw();
        }
      }
      shownId = pid;
      try {
        const d = await cache.get(want.url);
        if (id === pid && wanted(i, fit ? 0 : curScale)?.url === want.url) show(d, want.kind);
      } catch (e) {
        if (id === pid) {
          loadError = (e as Error).message;
          if (want.kind === 'raw') {
            // Fall back to the embedded JPEG so there is always something to judge.
            const j = urls.preview(pid, v, 2048);
            cache.get(j).then((d) => { if (id === pid) show(d, 'preview'); }).catch(() => {});
          }
        }
      }
    }
    shownId = pid;
    prefetch();
  }

  function prefetch() {
    const keep = new Set<string>();
    const n = store.settings.prefetch;
    const order = [];
    for (let k = 1; k <= n; k++) order.push(i + k);
    order.push(i - 1);
    for (const idx of order) {
      if (idx < 0 || idx >= L.id.length) continue;
      const w = wanted(idx, 0);
      if (!w) continue;
      keep.add(w.url);
      cache.get(w.url, true).catch(() => {});
    }
    if (shownUrl) keep.add(shownUrl);
    cache.cancelOthers(keep);
  }

  // The RAW clipping mask of the current photo (see MaskLoader): its overexposure display
  // follows the exposure correction.
  function loadMask() {
    const base = useRaw && id != null ? `${id}|${v}|${store.viewer.wb}` : '';
    const url = base ? urls.rawmask(id, v, store.viewer.wb, store.viewer.over ? ev : 0) : '';
    masks.update(base, url, store.viewer.over || store.viewer.under);
  }

  async function loadRawInfo() {
    if (id == null) {
      rawInfo = null;
      return;
    }
    if (!useRaw) {
      rawInfo = null;
      if (currentBmp && store.panels.histogram) computeJpegHist(currentBmp);
      return;
    }
    // Needed for the histogram and for the full-resolution view.
    if (!store.panels.histogram && store.settings.rawDetail === 'off') {
      rawInfo = null;
      return;
    }
    const pid = id;
    try {
      const r = await api.rawinfo(pid, v, store.viewer.wb);
      if (id === pid) rawInfo = r;
    } catch {
      rawInfo = null;
    }
  }

  function computeJpegHist(d: Decoded) {
    try {
      const w = 256;
      const h = Math.max(1, Math.round((d.h / d.w) * w));
      const oc = new OffscreenCanvas(w, h);
      const g = oc.getContext('2d')!;
      g.drawImage(d.bmp, 0, 0, w, h);
      const px = g.getImageData(0, 0, w, h).data;
      const r = new Uint32Array(256), gg = new Uint32Array(256), b = new Uint32Array(256);
      for (let k = 0; k < px.length; k += 4) {
        r[px[k]]++;
        gg[px[k + 1]]++;
        b[px[k + 2]]++;
      }
      jpegHist = [r, gg, b];
    } catch {
      jpegHist = null;
    }
  }

  // ---------------------------------------------------------------- drawing
  let raf = 0;
  function requestDraw() {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      draw();
    });
  }

  function clampCenter() {
    const s = curScale;
    const vw = cw / s;
    const vh = ch / s;
    cx = dispW <= vw ? dispW / 2 : Math.min(dispW - vw / 2, Math.max(vw / 2, cx));
    cy = dispH <= vh ? dispH / 2 : Math.min(dispH - vh / 2, Math.max(vh / 2, cy));
  }

  function draw() {
    if (!gl || !canvas) return;
    if (gl.resize()) {
      cw = canvas.width;
      ch = canvas.height;
    }
    if (fit) {
      cx = dispW / 2;
      cy = dispH / 2;
    } else clampCenter();
    const s = store.settings;
    if (detail) {
      if (!useRaw) syncImageDetail();
      detailOn = detail.update({ orientation: o, dispW, dispH, scale: curScale, cx, cy }, cw, ch);
      detailPath = detail.path;
    }
    if (store.panels.faces) {
      const dpr = window.devicePixelRatio || 1;
      const sc = curScale;
      const r = { x: (cw / 2 - cx * sc) / dpr, y: (ch / 2 - cy * sc) / dpr, w: (dispW * sc) / dpr, h: (dispH * sc) / dpr };
      if (!imgRect || Math.abs(imgRect.x - r.x) + Math.abs(imgRect.y - r.y) + Math.abs(imgRect.w - r.w) + Math.abs(imgRect.h - r.h) > 0.25) imgRect = r;
    }
    gl.draw({
      orientation: o,
      dispW,
      dispH,
      scale: curScale,
      cx,
      cy,
      sharpen: store.viewer.sharpen === 1 ? s.sharpen1 : store.viewer.sharpen === 2 ? s.sharpen2 : 0,
      sharpenSigma: SHARPEN_SIGMA[store.viewer.sharpen],
      invert: store.viewer.invert,
      peaking: store.viewer.peaking,
      peakColor: hexToRgb(s.peakColor),
      over: store.viewer.over,
      under: store.viewer.under,
      maskOnly: useRaw && masks.active,
      overSolid: s.overColor === 'red',
      underSolid: s.underDisplay === 'solid',
      underColor: hexToRgb(s.underColor),
      bg: s.bgGray ? [0.36, 0.36, 0.36] : [0.055, 0.07, 0.086],
      // RAW renders carry the correction already; JPEGs and thumbnails get it on screen.
      gain: shownKind === 'raw' ? 1 : 2 ** ev,
    });
  }

  // ---------------------------------------------------------------- zoom
  function setFit() {
    fit = true;
    requestDraw();
  }

  function zoomTo(ns: number, ax?: number, ay?: number) {
    const s0 = curScale;
    const min = fitScale * 0.5;
    const s1 = Math.min(16, Math.max(min, ns));
    const px = ax ?? cw / 2;
    const py = ay ?? ch / 2;
    if (fit) {
      cx = dispW / 2;
      cy = dispH / 2;
    }
    const ix = cx + (px - cw / 2) / s0;
    const iy = cy + (py - ch / 2) / s0;
    cx = ix - (px - cw / 2) / s1;
    cy = iy - (py - ch / 2) / s1;
    if (s1 <= fitScale * 1.0001) {
      fit = true;
    } else {
      fit = false;
      scale = s1;
    }
    clampCenter();
    requestDraw();
  }

  let lastPointer: [number, number] | null = null;
  function zoomToggle() {
    if (fit || curScale < 0.99) {
      const [ax, ay] = lastPointer ?? [cw / 2, ch / 2];
      zoomTo(1, ax, ay);
    } else setFit();
  }

  /** Space: fit → 100% → fit, about the centre. Steps not above fit are skipped, and from
   * any zoom past the last step it goes back to fit. */
  function zoomCycle() {
    const next = ZOOM_STEPS.find((z) => z > fitScale * 1.0001 && z > curScale * 1.001);
    if (next == null) setFit();
    else zoomTo(next);
  }

  /** Where the photo sits on screen at fit (CSS px), for the zoom from and to the grid. */
  function fittedRect(): Rect | null {
    if (!stage) return null;
    const r = stage.getBoundingClientRect();
    const s = Math.min(r.width / dispW, r.height / dispH);
    const w = dispW * s;
    const h = dispH * s;
    return { x: r.left + (r.width - w) / 2, y: r.top + (r.height - h) / 2, w, h };
  }

  /** The grid tile of a photo, if it is on screen. */
  function tileRect(pid: number): Rect | null {
    const el = document.querySelector<HTMLElement>(`.tile[data-id="${pid}"]`);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    if (r.bottom < 0 || r.top > innerHeight || r.width < 4) return null;
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  }

  onMount(() => {
    // Opened from the grid: the photo grows out of its tile.
    const from = id != null ? tileRect(id) : null;
    if (from) {
      requestAnimationFrame(() => {
        const to = fittedRect();
        if (to && id != null) flyImage(urls.thumb(id, v, store.thumbRev[id]), from, to, { duration: 380, radius: [6, 0], fadeOut: 240 });
      });
    }
    // Closing: the photo shrinks back into its tile once the grid is back.
    store.beforeCloseLoupe = () => {
      const pid = id;
      const fromRect = fittedRect();
      if (pid == null || !fromRect || !fit) return;
      const url = urls.thumb(pid, v, store.thumbRev[pid]);
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          const to = tileRect(pid);
          if (to) flyImage(url, fromRect, to, { duration: 320, radius: [0, 6], fadeOut: 120 });
        }),
      );
    };
    if (canvas) {
      try {
        gl = new GLView(canvas);
        cache.setMaxTexture(gl.maxTex);
        gl.resize();
        cw = canvas.width;
        ch = canvas.height;
      } catch (e) {
        glError = (e as Error).message;
      }
    }
    viewerCtl.zoomToggle = zoomToggle;
    viewerCtl.zoomCycle = zoomCycle;
    viewerCtl.fit = setFit;
    viewerCtl.zoomIn = () => zoomTo(curScale * 1.5);
    viewerCtl.zoomOut = () => zoomTo(curScale / 1.5);
    const ro = new ResizeObserver(() => {
      if (gl?.resize() && canvas) {
        cw = canvas.width;
        ch = canvas.height;
      }
      requestDraw();
    });
    if (stage) ro.observe(stage);
    canvas?.addEventListener('webglcontextlost', (e) => e.preventDefault());
    return () => {
      store.beforeCloseLoupe = null;
      ro.disconnect();
      cancelAnimationFrame(raf);
      cache.clear();
      masks.destroy();
      detail?.destroy();
      detail = null;
      gl?.destroy();
      gl = null;
    };
  });

  // Full-resolution RAW tiles: the demosaicing path follows the setting.
  $effect(() => {
    const want = store.settings.rawDetail;
    untrack(() => {
      if (!gl) return;
      const coarse = matchMedia('(pointer: coarse)').matches;
      const path: DetailPath | null =
        want === 'off' ? null : want === 'server' ? 'server' : want === 'gpu' || !coarse ? (DetailLayer.gpuSupported(gl.gl) ? 'gpu' : 'server') : 'server';
      if (detail && path === detail.path) return;
      detail?.destroy();
      detail = path ? new DetailLayer(gl, path, requestDraw) : null;
      detailOn = false;
      detailPath = detail?.path ?? null;
      syncDetail();
      requestDraw();
    });
  });

  /** Zoom tiles of the non-RAW view (see imageDetailSpec). Called on every draw. */
  function syncImageDetail() {
    if (!detail) return;
    const p = pyr !== 'loading' && pyr?.id === id && pyr.v === v ? pyr.p : null;
    const spec = id == null ? null : imageDetailSpec(p, { id, v, version: store.status?.version ?? '' }, { dispW, dispH, scale: curScale, orientation: o, fit });
    detail.setSpec(spec);
    const l = spec?.level ?? -1;
    if (imgLevel !== l) imgLevel = l;
  }

  function syncDetail() {
    if (!detail) return;
    if (!useRaw) {
      syncImageDetail();
      return;
    }
    const info = rawInfo;
    if (id == null || !info?.full || !info.tile) {
      detail.setSpec(null);
      return;
    }
    detail.setSpec({
      id,
      v,
      mode: store.viewer.tone,
      ev,
      wb: store.viewer.wb,
      full: info.full,
      tile: info.tile,
      apron: info.apron ?? 0,
      version: store.status?.version ?? '',
    });
  }

  $effect(() => {
    void rawInfo;
    void useRaw;
    void store.viewer.tone;
    void ev;
    untrack(() => {
      syncDetail();
      requestDraw();
    });
  });

  // The face overlay needs to know where the image is.
  $effect(() => {
    if (store.panels.faces) untrack(() => requestDraw());
  });

  // Reload when the photo, the source or any server-side render parameter changes.
  $effect(() => {
    void id;
    void store.viewer.source;
    void store.viewer.tone;
    void store.viewer.wb;
    void ev;
    void cw;
    void ch;
    void store.rev;
    untrack(() => load());
  });

  // Higher resolution when zooming in.
  $effect(() => {
    void fit;
    void scale;
    untrack(() => {
      const w = wanted(i, fit ? 0 : curScale);
      if (w && w.url !== shownUrl) load();
    });
  });

  // New photo: back to fit.
  let lastId = -1;
  $effect(() => {
    if (id !== lastId) {
      lastId = id;
      untrack(() => {
        fit = true;
        rawInfo = null;
      });
    }
  });

  $effect(() => {
    void store.viewer.over;
    void store.viewer.under;
    void useRaw;
    void id;
    void store.viewer.wb;
    void ev;
    untrack(() => loadMask());
  });

  $effect(() => {
    void store.panels.histogram;
    void store.settings.rawDetail;
    void useRaw;
    void id;
    void store.viewer.wb;
    untrack(() => loadRawInfo());
  });

  $effect(() => {
    void store.settings.rawDetail;
    void useRaw;
    void id;
    void v;
    untrack(() => loadPyramid());
  });

  // A pyramid that arrived (or failed) after zooming in: load what the zoom wants now.
  $effect(() => {
    void pyr;
    untrack(() => {
      if (!fit) load();
      requestDraw();
    });
  });

  $effect(() => {
    void store.viewer.sharpen;
    void store.viewer.invert;
    void store.viewer.peaking;
    void store.viewer.over;
    void store.viewer.under;
    void store.settings.bgGray;
    void store.settings.overColor;
    void store.settings.underDisplay;
    void store.settings.underColor;
    void store.settings.sharpen1;
    void store.settings.sharpen2;
    void o;
    void ev;
    requestDraw();
  });

  // ---------------------------------------------------------------- pointer input
  const pointers = new Map<number, { x: number; y: number }>();
  let dragStart: { x: number; y: number; cx: number; cy: number; t: number } | null = null;
  let pinch: { d: number; s: number; mx: number; my: number } | null = null;
  let lastTap = 0;
  let moved = false;

  function devXY(e: PointerEvent | WheelEvent | MouseEvent): [number, number] {
    const r = canvas!.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    return [(e.clientX - r.left) * dpr, (e.clientY - r.top) * dpr];
  }

  function onPointerDown(e: PointerEvent) {
    canvas?.setPointerCapture(e.pointerId);
    const [x, y] = devXY(e);
    pointers.set(e.pointerId, { x, y });
    moved = false;
    if (pointers.size === 1) {
      if (fit) {
        cx = dispW / 2;
        cy = dispH / 2;
      }
      dragStart = { x, y, cx, cy, t: performance.now() };
    } else if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), s: curScale, mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2 };
      dragStart = null;
    }
  }

  function onPointerMove(e: PointerEvent) {
    const [x, y] = devXY(e);
    lastPointer = [x, y];
    if (!pointers.has(e.pointerId)) return;
    pointers.set(e.pointerId, { x, y });
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y);
      zoomTo((pinch.s * d) / Math.max(1, pinch.d), pinch.mx, pinch.my);
      moved = true;
      return;
    }
    if (dragStart) {
      const dx = x - dragStart.x;
      const dy = y - dragStart.y;
      if (Math.abs(dx) + Math.abs(dy) > 6) moved = true;
      if (!fit) {
        cx = dragStart.cx - dx / curScale;
        cy = dragStart.cy - dy / curScale;
        requestDraw();
      }
    }
  }

  function onPointerUp(e: PointerEvent) {
    const [x, y] = devXY(e);
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (dragStart && pointers.size === 0) {
      const dx = x - dragStart.x;
      const dy = y - dragStart.y;
      const dt = performance.now() - dragStart.t;
      const dpr = window.devicePixelRatio || 1;
      if (fit && e.pointerType !== 'mouse' && Math.abs(dx) > 50 * dpr && Math.abs(dx) > Math.abs(dy) * 1.5 && dt < 700) {
        store.go(dx < 0 ? 1 : -1);
      } else if (!moved && e.pointerType !== 'mouse') {
        const now = performance.now();
        if (now - lastTap < 300) {
          lastPointer = [x, y];
          zoomToggle();
          lastTap = 0;
        } else {
          lastTap = now;
          setTimeout(() => {
            if (lastTap !== now) return;
            if (sheet === 'tools') sheet = null;
            else chrome = !chrome;
          }, 310);
        }
      }
      dragStart = null;
    }
  }

  let wheelLock = 0;
  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const [x, y] = devXY(e);
    lastPointer = [x, y];
    if (e.ctrlKey || e.metaKey || e.shiftKey) {
      const k = Math.exp(-(e.deltaY || e.deltaX) * (e.deltaMode === 1 ? 0.05 : 0.0025));
      zoomTo(curScale * k, x, y);
      return;
    }
    if (!fit) {
      cx += e.deltaX / curScale;
      cy += e.deltaY / curScale;
      requestDraw();
      return;
    }
    const now = performance.now();
    if (now < wheelLock || Math.abs(e.deltaY) < 4) return;
    wheelLock = now + 140;
    store.go(e.deltaY > 0 ? 1 : -1);
  }

  function onDbl(e: MouseEvent) {
    lastPointer = devXY(e);
    zoomToggle();
  }

  // ---------------------------------------------------------------- ui helpers
  const toneLabel: Record<string, string> = { normal: '', linear: 'Linear', shadows: 'Shadow boost', highlights: 'Highlight inspection' };
  const peakLabel = ['', 'Peaking: edges', 'Peaking: details', 'Peaking: both'];
  const wbLabel: Record<string, string> = { asshot: '', daylight: 'WB daylight', auto: 'WB auto' };
  const exif = $derived(store.detail && store.detail.id === id ? store.detail : null);
  const halfRes = $derived(shownKind === 'raw' && !fit && curScale > 0.55 && !detailOn);
  /** Whether zooming in further brings the full-resolution RAW view. */
  const fullResAhead = $derived(!!detailPath && !!rawInfo?.full);

  // ---------------------------------------------------------------- exposure
  const EV_STEP = 1 / 3;
  /** "+1⅓", "−⅔", "0": exposure in thirds of a stop. */
  function fmtEv(x: number): string {
    const t = Math.round(Math.abs(x) * 3);
    if (Math.abs(Math.abs(x) * 3 - t) > 0.01) return (x > 0 ? '+' : x < 0 ? '−' : '') + Math.abs(x).toFixed(2);
    if (t === 0) return '0';
    const whole = Math.floor(t / 3);
    const frac = ['', '⅓', '⅔'][t % 3];
    return (x > 0 ? '+' : '−') + (whole || !frac ? whole : '') + frac;
  }
  let evWheelAt = 0;
  function onEvWheel(e: WheelEvent) {
    e.preventDefault();
    const d = e.deltaY || e.deltaX;
    const now = performance.now();
    if (Math.abs(d) < 1 || now < evWheelAt) return;
    evWheelAt = now + 90;
    store.bumpEv(d < 0 ? EV_STEP : -EV_STEP);
  }

  // ---------------------------------------------------------------- mobile tools tray
  // While the tools are open the image stays live (pinch, pan, double-tap) and the page
  // itself must not zoom: lock the viewport and swallow browser pinch gestures outside
  // the canvas (iOS ignores user-scalable=no, but honours cancelled gesture events).
  const toolsOpen = $derived(store.mobile && sheet === 'tools');
  $effect(() => {
    if (!toolsOpen) return;
    const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
    const prev = meta?.content ?? '';
    if (meta) meta.content = `${prev}, maximum-scale=1, user-scalable=no`;
    const stop = (e: Event) => e.preventDefault();
    const multi = (e: TouchEvent) => {
      if (e.touches.length > 1 && e.target !== canvas) e.preventDefault();
    };
    document.addEventListener('gesturestart', stop);
    document.addEventListener('gesturechange', stop);
    document.addEventListener('touchmove', multi, { passive: false });
    return () => {
      if (meta) meta.content = prev;
      document.removeEventListener('gesturestart', stop);
      document.removeEventListener('gesturechange', stop);
      document.removeEventListener('touchmove', multi);
    };
  });
</script>

<div class="loupe" class:chrome-off={!chrome} class:no-zoom={toolsOpen}>
  <header class="top">
    <button class="btn ghost icon" onclick={() => store.closeLoupe()} title="Back to grid (G / Esc)"><Icon name="arrow-left" /></button>
    <div class="title">
      <div class="fname ellipsis">{L.n[i] ?? ''}</div>
      <div class="sub dim tnum">{[`${i + 1} / ${L.id.length}`, exif ? exif.folder || 'Library' : ''].filter(Boolean).join(' · ')}</div>
    </div>

    {#if !store.mobile}
      <div class="tools">
        <div class="seg" use:segmented title={'Switch RAW / embedded JPEG' + store.keyHint('source')}>
          <button class:on={!useRaw} onclick={() => store.setViewer('source', 'jpeg')}>JPEG</button>
          <button class:on={useRaw} disabled={!isRaw} onclick={() => store.setViewer('source', 'raw')}>RAW</button>
        </div>
        <button class="btn sm icon leveled" class:on={store.viewer.sharpen > 0} onclick={() => store.setViewer('sharpen', ((store.viewer.sharpen + 1) % 3) as 0 | 1 | 2)} title={'Screen sharpening' + store.keyHint('sharpen')}>
          <Icon name="sharpen" />{#if store.viewer.sharpen}<span class="level tnum">{store.viewer.sharpen}</span>{/if}
        </button>
        <button class="btn sm icon" class:on={store.viewer.peaking > 0} onclick={() => store.setViewer('peaking', ((store.viewer.peaking + 1) % 4) as 0 | 1 | 2 | 3)} title={'Focus peaking' + store.keyHint('peaking')}><Icon name="peaking" /></button>
        <button class="btn sm" class:on={store.viewer.over} onclick={() => store.setViewer('over', !store.viewer.over)} title={'Overexposed areas' + store.keyHint('over')}>OE</button>
        <button class="btn sm" class:on={store.viewer.under} onclick={() => store.setViewer('under', !store.viewer.under)} title={'Underexposed areas' + store.keyHint('under')}>UE</button>
        <span class="vsep"></span>
        <button class="btn sm icon" class:on={store.viewer.tone === 'shadows'} onclick={() => store.toggleTone('shadows')} title={'Shadow boost' + store.keyHint('shadows')}><Icon name="shadows" /></button>
        <button class="btn sm icon" class:on={store.viewer.tone === 'highlights'} onclick={() => store.toggleTone('highlights')} title={'Highlight inspection' + store.keyHint('highlights')}><Icon name="highlights" /></button>
        <button class="btn sm icon" class:on={store.viewer.tone === 'linear'} onclick={() => store.toggleTone('linear')} title={'Linear' + store.keyHint('linear')}><Icon name="linear" /></button>
        <button class="btn sm icon" class:on={store.viewer.invert} onclick={() => store.setViewer('invert', !store.viewer.invert)} title={'Invert colours' + store.keyHint('invert')}><Icon name="invert" /></button>
        <div class="evctl" onwheel={onEvWheel} role="group" aria-label="Exposure correction">
          <button class="btn sm icon" onclick={() => store.bumpEv(-EV_STEP)} title={'Exposure −⅓ EV' + store.keyHint('evDown')}><Icon name="minus" /></button>
          <button
            class="btn sm evval tnum"
            class:on={store.viewer.ev !== 0 && store.viewer.evOn}
            class:off={store.viewer.ev !== 0 && !store.viewer.evOn}
            onclick={() => store.setViewer('evOn', !store.viewer.evOn)}
            oncontextmenu={(e) => { e.preventDefault(); store.setViewer('ev', 0); }}
            title={'Exposure correction on/off' + store.keyHint('evToggle') + ' · scroll to adjust · right-click to reset' + store.keyHint('evReset')}
          >{fmtEv(store.viewer.ev)} EV</button>
          <button class="btn sm icon" onclick={() => store.bumpEv(EV_STEP)} title={'Exposure +⅓ EV' + store.keyHint('evUp')}><Icon name="plus" /></button>
        </div>
      </div>
    {/if}

    <div class="right">
      {#if !store.mobile}
        <button class="btn sm icon" class:on={store.panels.histogram} onclick={() => { store.panels.histogram = !store.panels.histogram; store.persist(); }} title="Histogram (F2 / H)"><Icon name="histogram" /></button>
        <button class="btn sm icon" onclick={() => store.rotate(-90)} title={'Rotate left' + store.keyHint('rotateCcw')}><Icon name="rotate-ccw" /></button>
        <button class="btn sm icon" onclick={() => store.rotate(90)} title={'Rotate right' + store.keyHint('rotateCw')}><Icon name="rotate-cw" /></button>
        <button class="btn sm icon" onclick={() => store.openLocation()} title={'Set location' + store.keyHint('location')}><Icon name="pin" /></button>
        {#if store.status?.immich}
          <button class="btn sm icon" onclick={() => store.openImmich(undefined, albums.current?.name)} title="Send to Immich"><Icon name="send" /></button>
        {/if}
        <button class="btn sm icon" onclick={() => albums.pick()} title={'Add to an album' + store.keyHint('albumPick') + (albums.byId(albums.target) ? ` · ${store.keyHint('albumAdd').trim()} adds to “${albums.byId(albums.target)?.name}”` : '')}><Icon name="album-plus" /></button>
        {#if people.enabled}
          <button class="btn sm icon" class:on={store.panels.faces} onclick={() => { store.panels.faces = !store.panels.faces; store.persist(); requestDraw(); }} title={'Faces and pets' + store.keyHint('faces')}><Icon name="scan-face" /></button>
        {/if}
        <button class="btn sm {inReject ? '' : 'danger'}" onclick={() => store.reject()} title={inReject ? 'Restore from reject folder' : 'Move to reject folder (Del / Ctrl+Shift+Del)'}>
          <Icon name={inReject ? 'restore' : 'reject'} /> {inReject ? 'Restore' : 'Reject'}
        </button>
        <button class="btn sm icon" class:on={store.panels.info} onclick={() => { store.panels.info = !store.panels.info; store.persist(); }} title="Info (I / F3)"><Icon name="info" /></button>
        <button class="btn sm icon" onclick={toggleFullscreen} title={'Full screen' + store.keyHint('fullscreen')}><Icon name="fullscreen" /></button>
      {:else}
        <button class="btn ghost icon" class:on={store.panels.histogram} onclick={() => (store.panels.histogram = !store.panels.histogram)} aria-label="Histogram"><Icon name="histogram" /></button>
        {#if people.enabled}
          <button class="btn ghost icon" class:on={store.panels.faces} onclick={() => { store.panels.faces = !store.panels.faces; store.persist(); requestDraw(); }} aria-label="Faces and pets"><Icon name="scan-face" /></button>
        {/if}
        <button class="btn ghost icon" onclick={() => (sheet = sheet === 'info' ? null : 'info')} aria-label="Info"><Icon name="info" /></button>
      {/if}
    </div>
  </header>

  <div class="body">
    <div class="stage" bind:this={stage}>
      <canvas
        bind:this={canvas}
        class:grab={!fit}
        onpointerdown={onPointerDown}
        onpointermove={onPointerMove}
        onpointerup={onPointerUp}
        onpointercancel={onPointerUp}
        onwheel={onWheel}
        ondblclick={onDbl}
      ></canvas>
      {#if glError}
        <div class="fallback">
          {#if id != null}<img src={urls.preview(id, v, 2048)} alt="" style:transform={o === 6 ? 'rotate(90deg)' : o === 8 ? 'rotate(-90deg)' : o === 3 ? 'rotate(180deg)' : ''} />{/if}
          <p class="dim">View tools need WebGL2 ({glError}).</p>
        </div>
      {/if}

      {#if store.panels.faces && id != null && people.enabled}
        <FaceOverlay photo={id} rect={imgRect} />
      {/if}

      {#if store.panels.histogram}
        <div class="histo">
          <Histogram raw={useRaw ? rawInfo : null} rgb={useRaw ? null : jpegHist} approximate={rawInfo?.approximate ?? false} />
        </div>
      {/if}

      <div class="chips">
        <span class="chip src" class:raw={shownKind === 'raw'}>{shownKind === 'raw' ? 'RAW' : fileLabel}{#if shownKind === 'thumb'}{' · loading'}{/if}</span>
        {#if halfRes}<span class="chip" title={fullResAhead ? 'RAW view at half resolution: zoom in to 75% or more for the full sensor resolution' : 'RAW view is rendered at half resolution; switch to JPEG' + store.keyHint('source') + ' for full-resolution focus checks'}>RAW ½ res</span>{/if}
        {#if toneLabel[store.viewer.tone] && useRaw}<span class="chip">{toneLabel[store.viewer.tone]}</span>{/if}
        {#if wbLabel[store.viewer.wb] && useRaw}<span class="chip">{wbLabel[store.viewer.wb]}</span>{/if}
        {#if ev !== 0}<span class="chip tnum">{fmtEv(ev)} EV{#if !useRaw}{' (JPEG)'}{/if}</span>{/if}
        {#if store.viewer.sharpen}<span class="chip">Sharpen {store.viewer.sharpen}</span>{/if}
        {#if store.viewer.invert}<span class="chip">Inverted</span>{/if}
        {#if store.viewer.peaking}<span class="chip">{peakLabel[store.viewer.peaking]}</span>{/if}
        {#if store.viewer.over}<span class="chip">OE{#if !useRaw}{' (JPEG)'}{/if}</span>{/if}
        {#if store.viewer.under}<span class="chip">UE{#if !useRaw}{' (JPEG)'}{/if}</span>{/if}
        {#if !fit}<span class="chip tnum" title={detailOn ? (useRaw ? 'Full sensor resolution, demosaiced ' + (detailPath === 'gpu' ? 'on this device' : 'on the server') : imgLevel === 0 ? 'Full-size tiles of the image' : `Tiles at 1/${2 ** imgLevel} size`) : ''}>{zoomPct}%{#if detailOn}{useRaw || imgLevel === 0 ? ' · full res' : ' · tiled'}{/if}</span>{/if}
        {#if loadError}<span class="chip err" title={loadError}>{shownKind === 'raw' ? 'RAW' : 'Image'} unavailable</span>{/if}
      </div>
      {#if inReject}<div class="rejected-banner">In {store.status?.reject_dir ?? '_Rejected'}</div>{/if}

      {#if !store.mobile}
        <button class="nav prev" onclick={() => store.go(-1)} disabled={i === 0} aria-label="Previous"><Icon name="chevron-left" size={28} /></button>
        <button class="nav next" onclick={() => store.go(1)} disabled={i >= L.id.length - 1} aria-label="Next"><Icon name="chevron" size={28} /></button>
      {/if}
    </div>

    {#if !store.mobile && store.panels.info && !store.panels.hidden}
      <aside class="side"><InfoPanel compact /></aside>
    {/if}
  </div>

  {#if !store.mobile}
    <footer class="bottom">
      <!-- Rebuilt per photo, so moving through photos never sets off the little pops. -->
      {#key id}
        <div class="rate">
          {#each [1, 2, 3, 4, 5] as r}
            <button class="star" class:on={cull.r >= r} style:--k={r} use:bump={cull.r >= r ? cull.r : null} onclick={() => store.rate(r)} title={`${r} star${r > 1 ? 's' : ''} (${r})`}>★</button>
          {/each}
        </div>
        <div class="labels">
          {#each LABELS as l}
            <button class="lbl" class:on={cull.lc === l.color} onclick={() => store.label(l.color)} title={`${l.name}${l.key ? ` (${l.key})` : ''}`}><span class="label-dot label-{l.color}" use:bump={cull.lc === l.color || null}></span></button>
          {/each}
        </div>
        <button class="btn sm pickbtn" class:pick={cull.p === 1} onclick={() => store.togglePick(1)} title={'Pick' + store.keyHint('pick')}>
          <span class="flagico" data-bump="wave-once" use:bump={cull.p === 1 || null}><Icon name="flag" fill={cull.p === 1} /></span>{cull.p === 1 ? 'Picked' : 'Pick'}
        </button>
      {/key}
      <div class="exif ellipsis dim tnum">
        {#if exif}
          {[cameraName(exif.make, exif.model), exif.lens, fmtSettings(exif)].filter(Boolean).join(' · ')}
        {/if}
      </div>
      <div class="zoom tnum">
        <button class="btn sm ghost" onclick={() => (fit ? zoomTo(1) : setFit())} title={'Fit / 100%' + store.keyHint('zoomToggle')}>{fit ? 'Fit' : `${zoomPct}%`}</button>
      </div>
    </footer>
    {#if store.panels.filmstrip && !store.panels.hidden}
      <div class="film" bind:clientWidth={filmW}>
        {#each filmSlots as j (j >= 0 && j < L.id.length ? L.id[j] : `g${j}`)}
          {@const _r = store.rev}
          <div class="fslot" animate:flip={{ duration: reduced() ? 0 : 280 }}>
            {#if j >= 0 && j < L.id.length}
              <button class="fthumb" class:cur={j === i} onclick={() => store.goTo(j)} title={L.n[j]}>
                <img src={urls.thumb(L.id[j], L.v[j], store.thumbRev[L.id[j]])} alt="" loading="lazy" draggable="false" />
                {#if L.r[j] > 0 && _r >= 0}<span class="fs">{'★'.repeat(L.r[j])}</span>{/if}
                {#if L.lc[j]}<span class="flc label-{L.lc[j]}"></span>{/if}
                {#if L.p[j] === 1}<span class="fpick"><Icon name="flag" size={10} fill /></span>{/if}
              </button>
            {:else}
              <span class="fthumb ghost" aria-hidden="true"></span>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {:else}
    {#if sheet === 'tools'}
      <!-- In the layout, not over it: the image shrinks to fit above and stays zoomable. -->
      <div class="tray">
        <div class="mtools">
          <button class="tool" class:on={store.viewer.sharpen > 0} onclick={() => store.setViewer('sharpen', ((store.viewer.sharpen + 1) % 3) as 0 | 1 | 2)}><Icon name="sharpen" /><span>Sharpen {store.viewer.sharpen || ''}</span></button>
          <button class="tool" class:on={store.viewer.peaking > 0} onclick={() => store.setViewer('peaking', ((store.viewer.peaking + 1) % 4) as 0 | 1 | 2 | 3)}><Icon name="peaking" /><span>Peaking</span></button>
          <button class="tool" class:on={store.viewer.over} onclick={() => store.setViewer('over', !store.viewer.over)}><Icon name="over" /><span>Overexposed</span></button>
          <button class="tool" class:on={store.viewer.under} onclick={() => store.setViewer('under', !store.viewer.under)}><Icon name="shadows" /><span>Underexposed</span></button>
          <button class="tool" class:on={store.viewer.tone === 'shadows'} onclick={() => store.toggleTone('shadows')}><Icon name="shadows" /><span>Shadow boost</span></button>
          <button class="tool" class:on={store.viewer.tone === 'highlights'} onclick={() => store.toggleTone('highlights')}><Icon name="highlights" /><span>Highlights</span></button>
          <button class="tool" class:on={store.viewer.tone === 'linear'} onclick={() => store.toggleTone('linear')}><Icon name="linear" /><span>Linear</span></button>
          <button class="tool" class:on={store.viewer.invert} onclick={() => store.setViewer('invert', !store.viewer.invert)}><Icon name="invert" /><span>Invert</span></button>
          <button class="tool" onclick={() => { const order = ['asshot', 'daylight', 'auto'] as const; store.setViewer('wb', order[(order.indexOf(store.viewer.wb) + 1) % 3]); store.setViewer('source', 'raw'); }}><Icon name="wb" /><span>WB {({ asshot: 'as shot', daylight: 'daylight', auto: 'auto' })[store.viewer.wb]}</span></button>
          <button class="tool" onclick={() => store.rotate(90)}><Icon name="rotate-cw" /><span>Rotate</span></button>
          <button class="tool" onclick={() => { sheet = null; store.openLocation(); }}><Icon name="pin" /><span>Location</span></button>
          <button class="tool" onclick={() => { sheet = null; albums.pick(); }}><Icon name="album-plus" /><span>Album</span></button>
          {#if store.status?.immich}
            <button class="tool" onclick={() => { sheet = null; store.openImmich(undefined, albums.current?.name); }}><Icon name="send" /><span>Immich</span></button>
          {/if}
        </div>
        <div class="evrow">
          <button class="btn" onclick={() => store.bumpEv(-EV_STEP)} aria-label="Exposure −⅓ EV"><Icon name="minus" /></button>
          <button class="btn ghost tnum evval" class:off={store.viewer.ev !== 0 && !store.viewer.evOn} onclick={() => store.setViewer('evOn', !store.viewer.evOn)}>{fmtEv(store.viewer.ev)} EV</button>
          <button class="btn" onclick={() => store.bumpEv(EV_STEP)} aria-label="Exposure +⅓ EV"><Icon name="plus" /></button>
          <button class="btn ghost" onclick={() => store.setViewer('ev', 0)}>Reset</button>
        </div>
      </div>
    {/if}
    <footer class="mbar">
      <button class="mb danger" onclick={() => store.reject()}><Icon name={inReject ? 'restore' : 'reject'} size={22} /><span>{inReject ? 'Restore' : 'Reject'}</span></button>
      <button class="mb" class:on={cull.r > 0} onclick={() => (sheet = sheet === 'rate' ? null : 'rate')}>
        <Icon name="star" size={22} fill={cull.r > 0} /><span>{cull.r > 0 ? `${cull.r}★` : 'Rate'}</span>
      </button>
      <button class="mb" class:pick={cull.p === 1} onclick={() => store.togglePick(1)}><Icon name="flag" size={22} fill={cull.p === 1} /><span>Pick</span></button>
      <button class="mb" onclick={() => isRaw && store.setViewer('source', useRaw ? 'jpeg' : 'raw')} disabled={!isRaw}>
        <span class="srcbadge" class:raw={useRaw}>{useRaw ? 'RAW' : fileLabel === 'JPEG' ? 'JPG' : fileLabel}</span><span>Source</span>
      </button>
      <button class="mb" class:on={sheet === 'tools'} onclick={() => (sheet = sheet === 'tools' ? null : 'tools')}><Icon name="sliders" size={22} /><span>Tools</span></button>
    </footer>
    {#if sheet === 'info' || sheet === 'rate'}
      <div class="sheet-scrim" onclick={() => (sheet = null)} role="presentation"></div>
      <div class="sheet" class:tall={sheet === 'info'}>
        <div class="grip"></div>
        {#if sheet === 'info'}
          <InfoPanel compact />
        {:else if sheet === 'rate'}
          <div class="mrate">
            {#each [1, 2, 3, 4, 5] as r}
              <button class="star big" class:on={cull.r >= r} onclick={() => { store.rate(r); sheet = null; }}>★</button>
            {/each}
          </div>
          <div class="mlabels">
            {#each LABELS as l}
              <button class="lbl big" class:on={cull.lc === l.color} onclick={() => { store.label(l.color); sheet = null; }} aria-label={l.name}><span class="label-dot label-{l.color}"></span></button>
            {/each}
          </div>
          <button class="btn" onclick={() => { store.rate(0); sheet = null; }}>Clear rating</button>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .loupe {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    flex-direction: column;
    background: var(--viewer-bg);
    color: var(--text);
    animation: loupe-in 0.24s var(--ease-out);
  }
  @keyframes loupe-in {
    from {
      opacity: 0;
    }
  }
  /* The viewer's chrome is graphite whatever the theme, so the photo is the brightest
     thing on screen. The info panel and the mobile sheets follow the theme. */
  .top,
  .bottom,
  .film,
  .mbar,
  .stage {
    --text: #ececf0;
    --text-2: #a4a8b1;
    --text-3: #737781;
    --surface: #16171c;
    --surface-2: #1d1f25;
    --surface-3: #282a32;
    --border: rgba(255, 255, 255, 0.07);
    --border-strong: rgba(255, 255, 255, 0.12);
    --field: rgba(255, 255, 255, 0.04);
    --field-border: rgba(255, 255, 255, 0.11);
    --accent: #8e9cff;
    --accent-strong: #6372ff;
    --accent-bg: rgba(110, 124, 255, 0.14);
    --accent-bg-strong: rgba(110, 124, 255, 0.22);
    --negative: #ff6b6b;
    --sheen: rgba(255, 255, 255, 0.05);
    color: var(--text);
  }
  .top {
    height: 3.25rem;
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0 0.625rem;
    padding-top: env(safe-area-inset-top);
    box-sizing: content-box;
    position: relative;
    z-index: 3;
    background: linear-gradient(180deg, #141519, #101115);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    animation: bar-down 0.34s var(--ease-out);
  }
  @keyframes bar-down {
    from {
      transform: translateY(-100%);
      opacity: 0;
    }
  }
  @keyframes bar-up {
    from {
      transform: translateY(100%);
      opacity: 0;
    }
  }
  .title {
    min-width: 0;
    flex: 0 1 18rem;
  }
  .fname {
    font-weight: 650;
    letter-spacing: -0.005em;
  }
  .sub {
    font-size: var(--fs-sm);
    margin-top: 1px;
  }
  .tools {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.1875rem;
    justify-content: center;
  }
  .tools .seg {
    margin-right: 0.5rem;
  }
  .tools .seg > button {
    height: 1.625rem;
    padding: 0 0.625rem;
    font-size: var(--fs-sm);
    font-weight: 650;
    letter-spacing: 0.03em;
  }
  /* Tool buttons stay quiet until they are switched on. */
  .tools .btn.sm,
  .right .btn.sm:not(.danger) {
    background: transparent;
    border-color: transparent;
    color: var(--text-2);
  }
  .tools .btn.sm:hover,
  .right .btn.sm:not(.danger):hover {
    background: rgba(255, 255, 255, 0.07);
    color: var(--text);
  }
  .tools .btn.sm.on,
  .right .btn.sm.on {
    background: var(--accent-bg-strong);
    color: var(--accent);
    box-shadow: inset 0 0 0 1px rgba(142, 156, 255, 0.28);
  }
  .right .btn.danger {
    background: rgba(255, 99, 99, 0.08);
    border-color: rgba(255, 99, 99, 0.24);
  }
  /* A tool with levels (sharpening 1 / 2): the icon stays centred in the square button,
     the level sits in its top-right corner. */
  .leveled {
    position: relative;
  }
  .leveled .level {
    position: absolute;
    top: 0.125rem;
    right: 0.1875rem;
    font-size: 0.5625rem;
    font-weight: 700;
    line-height: 1;
    animation: chip-in var(--dur) var(--ease-spring);
  }
  .evctl {
    display: flex;
    align-items: center;
    margin-left: 0.375rem;
    padding: 1px;
    border-radius: var(--r);
    background: rgba(255, 255, 255, 0.035);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.06);
  }
  .evctl .evval {
    min-width: 4.5rem;
    justify-content: center;
    font-weight: 600;
  }
  .evval.off {
    text-decoration: line-through;
    opacity: 0.6;
  }
  .vsep {
    width: 1px;
    height: 1.125rem;
    background: rgba(255, 255, 255, 0.09);
    margin: 0 0.375rem;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.1875rem;
    margin-left: auto;
  }
  .right .btn.danger {
    margin: 0 0.25rem;
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .stage {
    flex: 1;
    position: relative;
    min-width: 0;
    overflow: hidden;
  }
  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    touch-action: none;
    cursor: default;
  }
  canvas.grab {
    cursor: grab;
  }
  canvas.grab:active {
    cursor: grabbing;
  }
  .fallback {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
  }
  .fallback img {
    max-width: 100%;
    max-height: 100%;
  }
  .histo {
    position: absolute;
    top: 0.75rem;
    right: 0.75rem;
    z-index: 2;
    animation: pop-in var(--dur) var(--ease-out);
    transform-origin: top right;
  }
  .chips {
    position: absolute;
    left: 0.75rem;
    top: 0.75rem;
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
    max-width: calc(100% - 22rem);
    pointer-events: none;
  }
  .chips .chip {
    background: rgba(12, 13, 17, 0.62);
    backdrop-filter: blur(12px) saturate(1.4);
    -webkit-backdrop-filter: blur(12px) saturate(1.4);
    color: #ececf0;
    box-shadow:
      inset 0 0 0 1px rgba(255, 255, 255, 0.08),
      0 2px 10px rgba(0, 0, 0, 0.25);
    font-weight: 600;
  }
  .chip.src.raw {
    color: #ffb84d;
  }
  .chip.err {
    color: var(--reject);
  }
  .rejected-banner {
    position: absolute;
    left: 50%;
    top: 0.75rem;
    transform: translateX(-50%);
    background: rgba(240, 72, 72, 0.88);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    color: #fff;
    font-weight: 650;
    padding: 0.3125rem 0.875rem;
    border-radius: 1rem;
    font-size: var(--fs-sm);
    box-shadow: 0 10px 28px -8px rgba(255, 60, 60, 0.55);
    animation: banner-in 0.36s var(--ease-spring);
  }
  @keyframes banner-in {
    from {
      transform: translate(-50%, -160%);
      opacity: 0;
    }
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    width: 2.75rem;
    height: 4.5rem;
    border: 0;
    border-radius: var(--r-lg);
    background: rgba(12, 13, 17, 0.32);
    backdrop-filter: blur(10px);
    -webkit-backdrop-filter: blur(10px);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.06);
    color: #fff;
    cursor: pointer;
    opacity: 0;
    transition:
      opacity var(--dur),
      background-color var(--dur-fast),
      transform var(--dur) var(--ease-out);
    display: grid;
    place-items: center;
  }
  .stage:hover .nav:not(:disabled) {
    opacity: 0.85;
  }
  .nav:hover {
    background: rgba(12, 13, 17, 0.58);
  }
  .nav.prev {
    left: 0.75rem;
  }
  .nav.next {
    right: 0.75rem;
  }
  .nav.prev:hover {
    transform: translate(-2px, -50%);
  }
  .nav.next:hover {
    transform: translate(2px, -50%);
  }
  .nav:active {
    transform: translateY(-50%) scale(0.92);
  }
  .side {
    width: var(--panel-w);
    flex: none;
    overflow: auto;
    background: var(--surface);
    border-left: 1px solid var(--border);
    color: var(--text);
    animation: side-in 0.3s var(--ease-out);
  }
  @keyframes side-in {
    from {
      transform: translateX(1.5rem);
      opacity: 0;
    }
  }
  .bottom {
    height: 2.875rem;
    flex: none;
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0 0.75rem;
    position: relative;
    z-index: 2;
    background: linear-gradient(0deg, #141519, #101115);
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    animation: bar-up 0.34s var(--ease-out);
  }
  .rate {
    display: flex;
    gap: 1px;
  }
  .star {
    border: 0;
    background: none;
    color: color-mix(in srgb, var(--text) 17%, transparent);
    font-size: 1.1875rem;
    cursor: pointer;
    padding: 0 0.125rem;
    line-height: 1;
    transition:
      color var(--dur-fast),
      text-shadow var(--dur),
      transform var(--dur-fast) var(--ease-out);
  }
  .star:active {
    transform: scale(0.82);
  }
  .star.on {
    color: var(--star);
    text-shadow: 0 0 14px rgba(255, 201, 77, 0.38);
  }
  /* Setting a rating sends a little wave along the stars. */
  .star:global(.pop-once) {
    animation-delay: calc((var(--k, 1) - 1) * 38ms);
    animation-fill-mode: both;
  }
  /* Hover previews the rating: fill up to the hovered star, overriding the set rating. */
  @media (hover: hover) {
    .rate:has(.star:hover) .star {
      color: color-mix(in srgb, var(--text) 17%, transparent);
      text-shadow: none;
    }
    .rate:has(.star:hover) .star:hover,
    .rate:has(.star:hover) .star:has(~ .star:hover) {
      color: #ffd97a;
    }
  }
  .labels {
    display: flex;
    gap: 0.25rem;
  }
  .lbl {
    width: 1.5rem;
    height: 1.5rem;
    border-radius: 50%;
    border: 0;
    background: none;
    display: grid;
    place-items: center;
    cursor: pointer;
    box-shadow: inset 0 0 0 1.5px transparent;
    transition:
      box-shadow var(--dur),
      transform var(--dur-fast) var(--ease-out);
  }
  .lbl:hover {
    transform: scale(1.1);
  }
  .lbl.on {
    box-shadow: inset 0 0 0 1.5px rgba(255, 255, 255, 0.85);
  }
  .lbl .label-dot {
    width: 0.875rem;
    height: 0.875rem;
  }
  .pickbtn {
    min-width: 5.25rem;
  }
  .btn.pick {
    color: var(--pick);
    border-color: color-mix(in srgb, var(--pick) 55%, transparent);
    background: color-mix(in srgb, var(--pick) 12%, transparent);
  }
  .flagico {
    display: inline-flex;
  }
  .exif {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-sm);
    text-align: right;
  }
  .film {
    height: 5rem;
    flex: none;
    display: flex;
    gap: 0.3125rem;
    padding: 0.4375rem 0.5rem 0.5rem;
    background: #0b0c0f;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    overflow: hidden;
    justify-content: center;
    -webkit-mask: linear-gradient(90deg, transparent, #000 5%, #000 95%, transparent);
    mask: linear-gradient(90deg, transparent, #000 5%, #000 95%, transparent);
    animation: bar-up 0.4s var(--ease-out);
  }
  .fslot {
    height: 100%;
    flex: none;
  }
  .fthumb {
    display: block;
    height: 100%;
    aspect-ratio: 3 / 2;
    border: 0;
    border-radius: 0.375rem;
    padding: 0;
    background: #17181d;
    cursor: pointer;
    position: relative;
    overflow: hidden;
    opacity: 0.5;
    filter: saturate(0.75);
    transition:
      opacity var(--dur),
      filter var(--dur),
      transform var(--dur) var(--ease-out),
      box-shadow var(--dur);
  }
  .fthumb:hover {
    opacity: 0.9;
    filter: none;
    transform: translateY(-2px);
  }
  .fthumb.cur {
    opacity: 1;
    filter: none;
    box-shadow:
      0 0 0 2px var(--accent),
      0 6px 18px -6px rgba(99, 114, 255, 0.65);
  }
  .fthumb.ghost {
    visibility: hidden;
  }
  .fthumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .fs {
    position: absolute;
    left: 0.1875rem;
    bottom: 0.0625rem;
    font-size: 0.5625rem;
    letter-spacing: -0.5px;
    color: var(--star);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.9);
  }
  .flc {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 3px;
  }
  .fpick {
    position: absolute;
    right: 0.1875rem;
    bottom: 0.1875rem;
    display: flex;
    color: var(--pick);
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.8));
  }

  /* ---------- mobile ---------- */
  .mbar {
    flex: none;
    display: flex;
    justify-content: space-around;
    padding: 0.25rem 0.25rem calc(0.25rem + env(safe-area-inset-bottom));
    background: linear-gradient(0deg, #131418, #101115);
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    position: relative;
    z-index: 2;
    animation: bar-up 0.34s var(--ease-out);
  }
  .mb {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1875rem;
    padding: 0.375rem 0;
    border: 0;
    background: none;
    color: var(--text-2);
    font-size: 0.6875rem;
    font-weight: 600;
    border-radius: var(--r);
    transition:
      color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out);
  }
  .mb:active:not(:disabled) {
    transform: scale(0.88);
  }
  .mb.danger {
    color: var(--reject);
  }
  .mb.on {
    color: var(--star);
  }
  .mb.pick {
    color: var(--pick);
  }
  .mb:disabled {
    opacity: 0.4;
  }
  .srcbadge {
    height: 1.375rem;
    padding: 0 0.375rem;
    border-radius: 0.3125rem;
    border: 1.5px solid currentColor;
    display: grid;
    place-items: center;
    font-size: 0.6875rem;
    font-weight: 800;
    letter-spacing: 0.02em;
  }
  .srcbadge.raw {
    color: #ffb84d;
  }
  .loupe.no-zoom {
    touch-action: none;
  }
  .tray {
    flex: none;
    max-height: 45vh;
    overflow: auto;
    overscroll-behavior: contain;
    touch-action: pan-y;
    padding: 0.75rem 0.75rem 0.5rem;
    background: var(--surface);
    color: var(--text);
    border-top: 1px solid var(--border);
    animation: tray-up 0.3s var(--ease-out);
  }
  @keyframes tray-up {
    from {
      transform: translateY(40%);
      opacity: 0;
    }
  }
  .chrome-off .top,
  .chrome-off .mbar,
  .chrome-off .chips {
    display: none;
  }
  .sheet-scrim {
    position: fixed;
    inset: 0;
    z-index: 55;
    background: rgba(0, 0, 0, 0.38);
    animation: fade-in 0.22s ease-out;
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
  }
  .sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 56;
    background: var(--surface);
    color: var(--text);
    border-radius: 1.125rem 1.125rem 0 0;
    box-shadow: var(--shadow-2);
    padding: 0.5rem 1rem calc(1rem + env(safe-area-inset-bottom));
    max-height: 70vh;
    overflow: auto;
    animation: sheet-up 0.38s var(--ease-out);
  }
  .sheet.tall {
    max-height: 82vh;
    padding: 0.5rem 0 calc(1rem + env(safe-area-inset-bottom));
  }
  @keyframes sheet-up {
    from {
      transform: translateY(100%);
    }
  }
  .grip {
    width: 2.25rem;
    height: 0.3125rem;
    border-radius: 0.25rem;
    background: var(--border-strong);
    margin: 0.125rem auto 0.875rem;
  }
  .mrate,
  .mlabels {
    display: flex;
    justify-content: center;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }
  .star.big {
    font-size: 2.25rem;
    padding: 0 0.25rem;
  }
  .lbl.big {
    width: 2.75rem;
    height: 2.75rem;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .lbl.big .label-dot {
    width: 1.5rem;
    height: 1.5rem;
  }
  .lbl.big.on {
    box-shadow:
      inset 0 0 0 1px var(--accent),
      0 0 0 2px var(--accent);
  }
  .sheet > .btn {
    width: 100%;
  }
  .mtools {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 0.5rem;
  }
  .tool {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.3125rem;
    padding: 0.625rem 0.25rem;
    border-radius: var(--r-lg);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font-size: 0.6875rem;
    font-weight: 600;
    transition:
      background-color var(--dur-fast),
      border-color var(--dur-fast),
      color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out);
  }
  .tool:active {
    transform: scale(0.94);
  }
  .tool.on {
    background: var(--accent-bg-strong);
    border-color: var(--accent);
    color: var(--accent);
  }
  .evrow {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    margin-top: 0.75rem;
  }
  .evrow .evval {
    min-width: 5rem;
  }
  @media (max-width: 760px) {
    .top {
      height: 3rem;
    }
    .chips {
      max-width: calc(100% - 1.5rem);
      top: auto;
      bottom: 0.5rem;
    }
    .histo {
      top: 0.5rem;
      right: 0.5rem;
      transform: scale(0.85);
      transform-origin: top right;
    }
  }
</style>
