<script lang="ts" module>
  /** A pane's view: fit to the pane, or a zoom (device px per image px, 1 = 100%)
   * centred on a point given as a fraction of the displayed photo. */
  export interface PaneView {
    fit: boolean;
    scale: number;
    nx: number;
    ny: number;
  }

  /** Zoom commands for a pane (keyboard shortcuts). */
  export interface PaneCtl {
    zoomToggle: () => void;
    zoomCycle: () => void;
    zoomBy: (f: number) => void;
    /** The mouse is over this pane. */
    hovered: () => boolean;
  }
</script>

<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { store, LABELS, SHARPEN_SIGMA } from '../lib/store.svelte';
  import { ZOOM_STEPS } from '../lib/ctl';
  import { pointerOver } from '../lib/pointer';
  import { api, urls, type Pyramid, type RawInfo } from '../lib/api';
  import { GLView, hexToRgb } from '../lib/viewer/gl';
  import { type ImageCache, type Decoded } from '../lib/viewer/loader';
  import { DetailLayer, imageDetailSpec, type DetailPath } from '../lib/viewer/detail';
  import { MaskLoader } from '../lib/viewer/mask';
  import Icon from './Icon.svelte';

  let {
    id,
    active,
    view,
    cache,
    onview,
    onactivate,
    onremove,
    ctl = $bindable(),
  }: {
    id: number;
    active: boolean;
    view: PaneView;
    cache: ImageCache;
    onview: (v: PaneView) => void;
    onactivate: () => void;
    onremove: () => void;
    ctl?: PaneCtl;
  } = $props();

  let stage: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let gl: GLView | null = null;
  let detail: DetailLayer | null = null;
  let glError = $state('');
  let cw = $state(1);
  let ch = $state(1);
  let shownUrl = '';
  /** The shown image is a RAW render (exposure correction already applied). */
  let shownRaw = false;
  let shownId = -1;
  let loadError = $state('');
  const masks = new MaskLoader(() => gl, () => requestDraw());
  let rawInfo = $state<RawInfo | null>(null);
  let detailOn = $state(false);
  /** The zoom pyramid of the non-RAW view (see Loupe): 'loading', or null without one. */
  let pyr = $state<{ id: number; v: string; p: Pyramid | null } | 'loading' | null>(null);

  const L = $derived(store.list);
  const i = $derived(store.index.get(id) ?? -1);
  const v = $derived(L.v[i]);
  const o = $derived(L.o[i] ?? 1);
  const isRaw = $derived(L.k[i] === 1);
  const useRaw = $derived(isRaw && store.viewer.source === 'raw');
  const dispW = $derived((o >= 5 ? L.h[i] : L.w[i]) || 3);
  const dispH = $derived((o >= 5 ? L.w[i] : L.h[i]) || 2);
  const fitScale = $derived(Math.min(cw / dispW, ch / dispH));
  const curScale = $derived(view.fit ? fitScale : view.scale);
  const ev = $derived(store.viewer.evOn ? store.viewer.ev : 0);
  // In-place list edits are signalled by `rev` (see Loupe).
  const rating = $derived((void store.rev, L.r[i] ?? 0));
  const label = $derived((void store.rev, L.lc[i] ?? null));
  const pick = $derived((void store.rev, L.p[i] ?? 0));
  const inReject = $derived(!!store.folders.get(L.f[i])?.isReject);
  const name = $derived(L.n[i] ?? '');
  /** Copies share a name: the folder tells them apart (a duplicates group). */
  const where = $derived(
    (store.compare ?? []).some((c) => c !== id && L.n[store.index.get(c) ?? -1] === name) ? store.folders.get(L.f[i])?.path || 'Library' : '',
  );

  const SIZES = [1024, 1600, 2048, 2560, 3200, 4096];

  function wanted(): { url: string; kind: 'preview' | 'full' | 'raw' } | null {
    if (i < 0) return null;
    const need = Math.max(dispW, dispH) * curScale;
    if (useRaw) {
      const size = need > 4096 * 1.05 ? 0 : (SIZES.find((s) => s >= need) ?? 0);
      return { url: urls.raw(id, v, { w: size, mode: store.viewer.tone, ev, wb: store.viewer.wb }), kind: 'raw' };
    }
    // Zoomed in with zoom tiles: the base stays a preview of at most 4096 px.
    if (!view.fit && tilesUsable()) return { url: urls.preview(id, v, SIZES.find((s) => s >= need) ?? 4096), kind: 'preview' };
    if (need > 2600 || !view.fit) return { url: urls.jpeg(id, v), kind: 'full' };
    return { url: urls.preview(id, v, SIZES.find((s) => s >= need) ?? 2048), kind: 'preview' };
  }

  function tilesUsable(): boolean {
    if (!detail || useRaw || i < 0) return false;
    if (pyr === 'loading') return true;
    return pyr != null && pyr.id === id && pyr.v === v && pyr.p != null && pyr.p.levels > 0;
  }

  async function loadPyramid() {
    if (i < 0 || useRaw || !detail) {
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

  function show(d: Decoded, raw = false) {
    if (!gl) return;
    gl.setImage(d.bmp, d.w, d.h);
    shownUrl = d.url;
    shownRaw = raw;
    loadError = '';
    requestDraw();
  }

  async function load() {
    if (!gl || i < 0) return;
    const pid = id;
    const want = wanted();
    if (!want || shownUrl === want.url) return;
    const hit = cache.peek(want.url);
    if (hit) return show(hit, want.kind === 'raw');
    if (shownId !== pid) {
      // The thumbnail first (stored upright: only for unrotated photos).
      gl.setImage(null);
      requestDraw();
      if (o === 1) {
        const t = urls.thumb(pid, v, store.thumbRev[pid]);
        cache.get(t).then((d) => { if (id === pid && !shownUrl) show(d); }).catch(() => {});
      }
      shownUrl = '';
      shownId = pid;
    }
    try {
      const d = await cache.get(want.url);
      if (id === pid && wanted()?.url === want.url) show(d, want.kind === 'raw');
    } catch (e) {
      if (id !== pid) return;
      loadError = (e as Error).message;
      if (want.kind === 'raw') cache.get(urls.preview(pid, v, 2048)).then((d) => { if (id === pid) show(d); }).catch(() => {});
    }
  }

  // The RAW clipping mask (see MaskLoader): its overexposure display follows the correction.
  function loadMask() {
    const base = useRaw ? `${id}|${v}|${store.viewer.wb}` : '';
    const url = base ? urls.rawmask(id, v, store.viewer.wb, store.viewer.over ? ev : 0) : '';
    masks.update(base, url, store.viewer.over || store.viewer.under);
  }

  async function loadRawInfo() {
    rawInfo = null;
    if (!useRaw || store.settings.rawDetail === 'off') return;
    const pid = id;
    try {
      const r = await api.rawinfo(pid, v, store.viewer.wb);
      if (id === pid) rawInfo = r;
    } catch {
      /* no full-resolution view for this one */
    }
  }

  function syncDetail() {
    if (!detail) return;
    if (!useRaw) {
      const p = pyr !== 'loading' && pyr?.id === id && pyr.v === v ? pyr.p : null;
      return detail.setSpec(imageDetailSpec(p, { id, v, version: store.status?.version ?? '' }, { dispW, dispH, scale: curScale, orientation: o, fit: view.fit }));
    }
    if (!rawInfo?.full || !rawInfo.tile) return detail.setSpec(null);
    detail.setSpec({
      id,
      v,
      mode: store.viewer.tone,
      ev,
      wb: store.viewer.wb,
      full: rawInfo.full,
      tile: rawInfo.tile,
      apron: rawInfo.apron ?? 0,
      version: store.status?.version ?? '',
    });
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

  /** The view's centre in displayed image px, kept inside the photo. */
  function center(s: number): [number, number] {
    if (view.fit) return [dispW / 2, dispH / 2];
    const vw = cw / s;
    const vh = ch / s;
    const cx = dispW <= vw ? dispW / 2 : Math.min(dispW - vw / 2, Math.max(vw / 2, view.nx * dispW));
    const cy = dispH <= vh ? dispH / 2 : Math.min(dispH - vh / 2, Math.max(vh / 2, view.ny * dispH));
    return [cx, cy];
  }

  function draw() {
    if (!gl || !canvas) return;
    if (gl.resize()) {
      cw = canvas.width;
      ch = canvas.height;
    }
    const s = store.settings;
    const scale = curScale;
    const [cx, cy] = center(scale);
    if (detail) {
      if (!useRaw) syncDetail();
      detailOn = detail.update({ orientation: o, dispW, dispH, scale, cx, cy }, cw, ch);
    }
    gl.draw({
      orientation: o,
      dispW,
      dispH,
      scale,
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
      gain: shownRaw ? 1 : 2 ** ev,
    });
  }

  // ---------------------------------------------------------------- zoom and pan
  /** A new view zoomed to `ns` about the device point (ax, ay). */
  function zoomed(ns: number, ax = cw / 2, ay = ch / 2): PaneView {
    const s0 = curScale;
    const s1 = Math.min(16, Math.max(fitScale * 0.5, ns));
    const [cx, cy] = center(s0);
    const ix = cx + (ax - cw / 2) / s0;
    const iy = cy + (ay - ch / 2) / s0;
    if (s1 <= fitScale * 1.0001) return { fit: true, scale: s1, nx: 0.5, ny: 0.5 };
    return { fit: false, scale: s1, nx: (ix - (ax - cw / 2) / s1) / dispW, ny: (iy - (ay - ch / 2) / s1) / dispH };
  }

  /** Where the zoom keys zoom about: the mouse when it is over this pane, else the middle. */
  function keyAnchor(): [number, number] {
    return pointerOver(canvas) ?? [cw / 2, ch / 2];
  }
  /** Z or a double tap: fit ↔ 100%, about the mouse or the tap. */
  function zoomToggle([ax, ay] = keyAnchor()) {
    onview(view.fit || curScale < 0.99 ? zoomed(1, ax, ay) : { fit: true, scale: fitScale, nx: 0.5, ny: 0.5 });
  }
  /** Space: fit → 100% → fit, about the mouse like Z (as in the single view). */
  function zoomCycle() {
    const next = ZOOM_STEPS.find((z) => z > fitScale * 1.0001 && z > curScale * 1.001);
    onview(next == null ? { fit: true, scale: fitScale, nx: 0.5, ny: 0.5 } : zoomed(next, ...keyAnchor()));
  }
  ctl = {
    zoomToggle: () => zoomToggle(),
    zoomCycle,
    zoomBy: (f: number) => onview(zoomed(curScale * f)),
    hovered: () => pointerOver(canvas) != null,
  };

  function devXY(e: PointerEvent | WheelEvent | MouseEvent): [number, number] {
    const r = canvas!.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    return [(e.clientX - r.left) * dpr, (e.clientY - r.top) * dpr];
  }

  const pointers = new Map<number, { x: number; y: number }>();
  let drag: { x: number; y: number; cx: number; cy: number } | null = null;
  let pinch: { d: number; s: number; mx: number; my: number } | null = null;
  let lastTap = 0;

  function onPointerDown(e: PointerEvent) {
    if (!active) onactivate();
    canvas?.setPointerCapture(e.pointerId);
    const [x, y] = devXY(e);
    pointers.set(e.pointerId, { x, y });
    if (pointers.size === 1) {
      const [cx, cy] = center(curScale);
      drag = { x, y, cx, cy };
    } else if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), s: curScale, mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2 };
      drag = null;
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (!pointers.has(e.pointerId)) return;
    const [x, y] = devXY(e);
    pointers.set(e.pointerId, { x, y });
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      onview(zoomed((pinch.s * Math.hypot(a.x - b.x, a.y - b.y)) / Math.max(1, pinch.d), pinch.mx, pinch.my));
      return;
    }
    if (drag && !view.fit) {
      const s = curScale;
      onview({ fit: false, scale: s, nx: (drag.cx - (x - drag.x) / s) / dispW, ny: (drag.cy - (y - drag.y) / s) / dispH });
    }
  }

  function onPointerUp(e: PointerEvent) {
    const [x, y] = devXY(e);
    const wasDrag = drag && Math.abs(x - drag.x) + Math.abs(y - drag.y) > 6;
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size === 0) {
      drag = null;
      if (!wasDrag) {
        const now = performance.now();
        if (now - lastTap < 320) {
          zoomToggle([x, y]);
          lastTap = 0;
        } else lastTap = now;
      }
    }
  }

  // As in the single view: Ctrl / Shift + wheel zooms, the wheel pans when zoomed in,
  // and steps this pane's photo through the folder at fit.
  let wheelLock = 0;
  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (!active) onactivate();
    const [x, y] = devXY(e);
    if (e.ctrlKey || e.metaKey || e.shiftKey) {
      onview(zoomed(curScale * Math.exp(-(e.deltaY || e.deltaX) * (e.deltaMode === 1 ? 0.05 : 0.0025)), x, y));
      return;
    }
    if (!view.fit) {
      const s = curScale;
      const [cx, cy] = center(s);
      onview({ fit: false, scale: s, nx: (cx + e.deltaX / s) / dispW, ny: (cy + e.deltaY / s) / dispH });
      return;
    }
    const now = performance.now();
    if (now < wheelLock || Math.abs(e.deltaY) < 4) return;
    wheelLock = now + 140;
    store.compareStep(e.deltaY > 0 ? 1 : -1);
  }

  function rate(r: number) {
    if (!active) onactivate();
    store.rate(r);
  }

  onMount(() => {
    if (canvas) {
      try {
        gl = new GLView(canvas);
        gl.resize();
        cw = canvas.width;
        ch = canvas.height;
      } catch (e) {
        glError = (e as Error).message;
      }
    }
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
      ro.disconnect();
      cancelAnimationFrame(raf);
      masks.destroy();
      detail?.destroy();
      detail = null;
      gl?.destroy();
      gl = null;
    };
  });

  // Full-resolution RAW tiles, as in the single view.
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
      syncDetail();
      requestDraw();
    });
  });

  $effect(() => {
    void id;
    void store.viewer.source;
    void store.viewer.tone;
    void store.viewer.wb;
    void ev;
    void cw;
    void ch;
    void store.rev;
    void view.fit;
    void view.scale;
    untrack(() => load());
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
    void store.settings.rawDetail;
    void useRaw;
    void id;
    void v;
    untrack(() => loadPyramid());
  });

  $effect(() => {
    void pyr;
    untrack(() => {
      if (!view.fit) load();
      requestDraw();
    });
  });

  $effect(() => {
    void useRaw;
    void id;
    void store.viewer.wb;
    void store.settings.rawDetail;
    untrack(() => loadRawInfo());
  });

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

  $effect(() => {
    void view.fit;
    void view.scale;
    void view.nx;
    void view.ny;
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
    void dispW;
    void ev;
    requestDraw();
  });
</script>

<div class="pane" class:active bind:this={stage}>
  {#if glError}
    <div class="fallback"><img src={urls.preview(id, v, 2048)} alt={name} /></div>
  {:else}
    <canvas
      bind:this={canvas}
      class:grab={!view.fit}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={onPointerUp}
      onwheel={onWheel}
    ></canvas>
  {/if}
  <div class="head">
    <span class="name" title={where ? `${where}/${name}` : name}>{name}</span>
    {#if where}<span class="where" title={where}>{where}</span>{/if}
    {#if !view.fit}<span class="chip tnum">{Math.round(curScale * 100)}%{#if detailOn}{' · full res'}{/if}</span>{/if}
    {#if useRaw}<span class="chip raw">RAW</span>{/if}
    {#if loadError}<span class="chip err" title={loadError}>Unavailable</span>{/if}
    <button class="close" onclick={onremove} title="Remove from the comparison" aria-label="Remove {name} from the comparison"><Icon name="x" size={16} /></button>
  </div>
  {#if inReject}<div class="rejected">In {store.status?.reject_dir ?? '_Rejected'}</div>{/if}
  <div class="foot">
    <div class="stars" role="group" aria-label="Rating">
      {#each [1, 2, 3, 4, 5] as r}
        <button class:on={rating >= r} onclick={() => rate(r)} title={`${r}★`} aria-label={`Rate ${r}`}>★</button>
      {/each}
    </div>
    {#if label}<span class="label-dot label-{label}" title={LABELS.find((l) => l.color === label)?.name}></span>{/if}
    <span class="grow"></span>
    <button class="act" class:on={pick === 1} onclick={() => { if (!active) onactivate(); store.togglePick(1); }} title="Pick" aria-pressed={pick === 1}>
      <Icon name="flag" size={16} /><span>Pick</span>
    </button>
    <button class="act danger" onclick={() => { if (!active) onactivate(); store.reject([id]); }} title={inReject ? 'Restore' : 'Reject (move to the reject folder)'}>
      <Icon name={inReject ? 'restore' : 'reject'} size={16} /><span>{inReject ? 'Restore' : 'Reject'}</span>
    </button>
  </div>
</div>

<style>
  .pane {
    position: relative;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--viewer-bg);
  }
  /* The active pane's frame, above the canvas. */
  .pane::after {
    content: '';
    position: absolute;
    inset: 0;
    border: 2px solid transparent;
    pointer-events: none;
    transition: border-color 0.12s;
    z-index: 2;
  }
  .pane.active::after {
    border-color: var(--accent);
  }
  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    touch-action: none;
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
  .head,
  .foot {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 0.375rem;
    padding: 0.375rem 0.5rem;
    pointer-events: none;
  }
  .head {
    top: 0;
    background: linear-gradient(rgba(10, 13, 16, 0.7), rgba(10, 13, 16, 0));
  }
  .foot {
    bottom: 0;
    background: linear-gradient(rgba(10, 13, 16, 0), rgba(10, 13, 16, 0.75));
  }
  .head > *,
  .foot > * {
    pointer-events: auto;
  }
  .grow {
    flex: 1;
    pointer-events: none;
  }
  .name {
    font-weight: 600;
    font-size: var(--fs-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 0 1 auto;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  }
  .where {
    font-size: var(--fs-sm);
    color: #b9c3cc;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 0 1 auto;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  }
  .chip {
    font-size: 0.6875rem;
    padding: 0.0625rem 0.4375rem;
    border-radius: 1rem;
    background: rgba(14, 18, 22, 0.75);
    color: #dfe6ec;
    white-space: nowrap;
  }
  .chip.raw {
    color: #ffc04d;
    font-weight: 700;
  }
  .chip.err {
    color: #ff8a80;
  }
  .close {
    margin-left: auto;
    border: 0;
    background: rgba(14, 18, 22, 0.6);
    color: #dfe6ec;
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    display: grid;
    place-items: center;
    cursor: pointer;
    flex: none;
  }
  .close:hover {
    background: rgba(40, 48, 56, 0.9);
  }
  .stars button {
    border: 0;
    background: none;
    color: rgba(255, 255, 255, 0.35);
    font-size: 1.125rem;
    cursor: pointer;
    padding: 0 0.0625rem;
    line-height: 1;
  }
  .stars button.on {
    color: var(--star);
  }
  .stars button:hover {
    transform: scale(1.15);
  }
  /* Hover previews the rating: fill up to the hovered star, overriding the set rating. */
  @media (hover: hover) {
    .stars:has(button:hover) button {
      color: rgba(255, 255, 255, 0.35);
    }
    .stars:has(button:hover) button:hover,
    .stars:has(button:hover) button:has(~ button:hover) {
      color: #ffd97a;
    }
  }
  .act {
    display: inline-flex;
    align-items: center;
    gap: 0.3125rem;
    height: 1.875rem;
    padding: 0 0.625rem;
    border-radius: 0.9375rem;
    border: 1px solid rgba(255, 255, 255, 0.18);
    background: rgba(14, 18, 22, 0.6);
    color: #e8edf2;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .act:hover {
    background: rgba(40, 48, 56, 0.9);
  }
  .act.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .act.danger:hover {
    background: #8b2a26;
    border-color: #b23a33;
  }
  .rejected {
    position: absolute;
    left: 50%;
    top: 2.5rem;
    transform: translateX(-50%);
    background: rgba(139, 42, 38, 0.85);
    color: #fff;
    font-size: var(--fs-sm);
    padding: 0.1875rem 0.625rem;
    border-radius: 1rem;
    pointer-events: none;
  }
  @media (max-width: 760px) {
    .act span {
      display: none;
    }
    .act {
      padding: 0 0.5rem;
    }
  }
</style>
