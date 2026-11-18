<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { store, LABELS, fmtSettings, cameraName, SHARPEN_SIGMA } from '../lib/store.svelte';
  import { api, urls, type RawInfo } from '../lib/api';
  import { GLView, hexToRgb } from '../lib/viewer/gl';
  import { ImageCache, type Decoded } from '../lib/viewer/loader';
  import { viewerCtl, toggleFullscreen } from '../lib/ctl';
  import Icon from './Icon.svelte';
  import Histogram from './Histogram.svelte';
  import InfoPanel from './InfoPanel.svelte';

  let stage: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let gl: GLView | null = null;
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
  let maskShown = '';
  let rawInfo = $state<RawInfo | null>(null);
  let jpegHist = $state<Uint32Array[] | null>(null);
  let chrome = $state(true);
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
  const ev = $derived(store.viewer.evOn ? store.viewer.ev : 0);
  const fitScale = $derived(Math.min(cw / dispW, ch / dispH));
  const curScale = $derived(fit ? fitScale : scale);
  const zoomPct = $derived(Math.round(curScale * 100));
  const inReject = $derived(!!store.folders.get(L.f[i])?.isReject);

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
    if (need > 2600 || forScale > 0) return { url: urls.jpeg(pid, L.v[idx]), kind: 'full' };
    return { url: urls.preview(pid, L.v[idx], SIZES.find((s) => s >= need) ?? 2048), kind: 'preview' };
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

  // The RAW clipping mask of the current photo. Kept while OE/UE are toggled off so
  // toggling back is instant; dropped as soon as the photo, WB or source changes so a
  // previous photo's mask is never drawn over this one. While it loads, RAW view shows
  // no over/underexposure at all rather than a misleading guess from the rendered image.
  async function loadMask() {
    if (!gl) return;
    const url = useRaw && id != null ? urls.rawmask(id, v, store.viewer.wb) : '';
    if (url === maskShown) return;
    if (maskShown) {
      maskShown = '';
      gl.setMask(null);
      requestDraw();
    }
    if (!url || !(store.viewer.over || store.viewer.under)) return;
    maskShown = url;
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(res.statusText);
      const bmp = await createImageBitmap(await res.blob(), { colorSpaceConversion: 'none', premultiplyAlpha: 'none' });
      if (!gl || maskShown !== url) return bmp.close();
      gl.setMask(bmp);
      bmp.close();
      requestDraw();
    } catch {
      // No RAW mask (e.g. the RAW failed to develop): fall back to the shown image.
      if (gl && maskShown === url) {
        maskShown = '';
        gl.setMask(null);
        requestDraw();
      }
    }
  }

  async function loadRawInfo() {
    if (!store.panels.histogram || id == null) {
      rawInfo = null;
      return;
    }
    if (!useRaw) {
      rawInfo = null;
      if (currentBmp) computeJpegHist(currentBmp);
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
      maskOnly: useRaw && maskShown !== '',
      overSolid: s.overColor === 'red',
      underSolid: s.underDisplay === 'solid',
      underColor: hexToRgb(s.underColor),
      bg: s.bgGray ? [0.36, 0.36, 0.36] : [0.055, 0.07, 0.086],
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

  onMount(() => {
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
      ro.disconnect();
      cancelAnimationFrame(raf);
      cache.clear();
      gl?.destroy();
      gl = null;
    };
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
    untrack(() => loadMask());
  });

  $effect(() => {
    void store.panels.histogram;
    void useRaw;
    void id;
    void store.viewer.wb;
    untrack(() => loadRawInfo());
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
            if (lastTap === now) chrome = !chrome;
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
  const halfRes = $derived(shownKind === 'raw' && !fit && curScale > 0.55);
</script>

<div class="loupe" class:chrome-off={!chrome}>
  <header class="top">
    <button class="btn ghost icon" onclick={() => store.closeLoupe()} title="Back to grid (G / Esc)"><Icon name="arrow-left" /></button>
    <div class="title">
      <div class="fname ellipsis">{L.n[i] ?? ''}</div>
      <div class="sub dim tnum">{[`${i + 1} / ${L.id.length}`, exif ? exif.folder || 'Library' : ''].filter(Boolean).join(' · ')}</div>
    </div>

    {#if !store.mobile}
      <div class="tools">
        <div class="seg" title="Switch RAW / embedded JPEG (J)">
          <button class:on={!useRaw} onclick={() => store.setViewer('source', 'jpeg')}>JPEG</button>
          <button class:on={useRaw} disabled={!isRaw} onclick={() => store.setViewer('source', 'raw')}>RAW</button>
        </div>
        <button class="btn sm icon" class:on={store.viewer.sharpen > 0} onclick={() => store.setViewer('sharpen', ((store.viewer.sharpen + 1) % 3) as 0 | 1 | 2)} title="Screen sharpening (S)">
          <Icon name="sharpen" />{#if store.viewer.sharpen}<sup>{store.viewer.sharpen}</sup>{/if}
        </button>
        <button class="btn sm icon" class:on={store.viewer.peaking > 0} onclick={() => store.setViewer('peaking', ((store.viewer.peaking + 1) % 4) as 0 | 1 | 2 | 3)} title="Focus peaking (P)"><Icon name="peaking" /></button>
        <button class="btn sm" class:on={store.viewer.over} onclick={() => store.setViewer('over', !store.viewer.over)} title="Overexposed areas (O)">OE</button>
        <button class="btn sm" class:on={store.viewer.under} onclick={() => store.setViewer('under', !store.viewer.under)} title="Underexposed areas (U)">UE</button>
        <span class="vsep"></span>
        <button class="btn sm icon" class:on={store.viewer.tone === 'shadows'} onclick={() => store.toggleTone('shadows')} title="Shadow boost (Shift+S)"><Icon name="shadows" /></button>
        <button class="btn sm icon" class:on={store.viewer.tone === 'highlights'} onclick={() => store.toggleTone('highlights')} title="Highlight inspection (Shift+H)"><Icon name="highlights" /></button>
        <button class="btn sm icon" class:on={store.viewer.tone === 'linear'} onclick={() => store.toggleTone('linear')} title="Linear (Shift+L)"><Icon name="linear" /></button>
        <button class="btn sm icon" class:on={store.viewer.invert} onclick={() => store.setViewer('invert', !store.viewer.invert)} title="Invert colours (Shift+I)"><Icon name="invert" /></button>
        <button class="btn sm" class:on={store.viewer.ev !== 0 && store.viewer.evOn} onclick={() => store.setViewer('evOn', !store.viewer.evOn)} title="Exposure correction (A, Alt+= / Alt+−)">
          {store.viewer.ev > 0 ? '+' : ''}{store.viewer.ev.toFixed(1)} EV
        </button>
      </div>
    {/if}

    <div class="right">
      {#if !store.mobile}
        <button class="btn sm icon" class:on={store.panels.histogram} onclick={() => { store.panels.histogram = !store.panels.histogram; store.persist(); }} title="Histogram (F2 / H)"><Icon name="histogram" /></button>
        <button class="btn sm icon" onclick={() => store.rotate(-90)} title="Rotate left (Ctrl+[)"><Icon name="rotate-ccw" /></button>
        <button class="btn sm icon" onclick={() => store.rotate(90)} title="Rotate right (Ctrl+])"><Icon name="rotate-cw" /></button>
        <button class="btn sm icon" onclick={() => store.openLocation()} title="Set location (L)"><Icon name="pin" /></button>
        <button class="btn sm {inReject ? '' : 'danger'}" onclick={() => store.reject()} title={inReject ? 'Restore from reject folder' : 'Move to reject folder (Del / Ctrl+Shift+Del)'}>
          <Icon name={inReject ? 'restore' : 'reject'} /> {inReject ? 'Restore' : 'Reject'}
        </button>
        <button class="btn sm icon" class:on={store.panels.info} onclick={() => { store.panels.info = !store.panels.info; store.persist(); }} title="Info (I / F3)"><Icon name="info" /></button>
        <button class="btn sm icon" onclick={toggleFullscreen} title="Full screen (F)"><Icon name="fullscreen" /></button>
      {:else}
        <button class="btn ghost icon" class:on={store.panels.histogram} onclick={() => (store.panels.histogram = !store.panels.histogram)} aria-label="Histogram"><Icon name="histogram" /></button>
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

      {#if store.panels.histogram}
        <div class="histo">
          <Histogram raw={useRaw ? rawInfo : null} rgb={useRaw ? null : jpegHist} approximate={rawInfo?.approximate ?? false} />
        </div>
      {/if}

      <div class="chips">
        <span class="chip src" class:raw={shownKind === 'raw'}>{shownKind === 'raw' ? 'RAW' : fileLabel}{#if shownKind === 'thumb'} · loading{/if}</span>
        {#if halfRes}<span class="chip" title="RAW view is rendered at half resolution; switch to JPEG (J) for full-resolution focus checks">RAW ½ res</span>{/if}
        {#if toneLabel[store.viewer.tone] && useRaw}<span class="chip">{toneLabel[store.viewer.tone]}</span>{/if}
        {#if wbLabel[store.viewer.wb] && useRaw}<span class="chip">{wbLabel[store.viewer.wb]}</span>{/if}
        {#if store.viewer.sharpen}<span class="chip">Sharpen {store.viewer.sharpen}</span>{/if}
        {#if store.viewer.invert}<span class="chip">Inverted</span>{/if}
        {#if store.viewer.peaking}<span class="chip">{peakLabel[store.viewer.peaking]}</span>{/if}
        {#if store.viewer.over}<span class="chip">OE{#if !useRaw} (JPEG){/if}</span>{/if}
        {#if store.viewer.under}<span class="chip">UE{#if !useRaw} (JPEG){/if}</span>{/if}
        {#if !fit}<span class="chip tnum">{zoomPct}%</span>{/if}
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
      <div class="rate">
        {#each [1, 2, 3, 4, 5] as r}
          <button class="star" class:on={(L.r[i] ?? 0) >= r} onclick={() => store.rate(r)} title={`${r} star${r > 1 ? 's' : ''} (${r})`}>★</button>
        {/each}
      </div>
      <div class="labels">
        {#each LABELS as l}
          <button class="lbl" class:on={L.lc[i] === l.color} onclick={() => store.label(l.color)} title={`${l.name}${l.key ? ` (${l.key})` : ''}`}><span class="label-dot label-{l.color}"></span></button>
        {/each}
      </div>
      <button class="btn sm" class:pick={L.p[i] === 1} onclick={() => store.togglePick(1)} title="Pick (Alt+P)"><Icon name="flag" fill={L.p[i] === 1} />{L.p[i] === 1 ? 'Picked' : 'Pick'}</button>
      <div class="exif ellipsis dim tnum">
        {#if exif}
          {[cameraName(exif.make, exif.model), exif.lens, fmtSettings(exif)].filter(Boolean).join(' · ')}
        {/if}
      </div>
      <div class="zoom tnum">
        <button class="btn sm ghost" onclick={() => (fit ? zoomTo(1) : setFit())} title="Fit / 100% (Z)">{fit ? 'Fit' : `${zoomPct}%`}</button>
      </div>
    </footer>
    {#if store.panels.filmstrip && !store.panels.hidden}
      <div class="film" bind:clientWidth={filmW}>
        {#each { length: filmHalf * 2 + 1 } as _, k}
          {@const j = i - filmHalf + k}
          {#if j >= 0 && j < L.id.length}
            <button class="fthumb" class:cur={j === i} onclick={() => store.goTo(j)} title={L.n[j]}>
              <img src={urls.thumb(L.id[j], L.v[j], store.thumbRev[L.id[j]])} alt="" loading="lazy" draggable="false" />
              {#if L.r[j] > 0}<span class="fs">{'★'.repeat(L.r[j])}</span>{/if}
              {#if L.lc[j]}<span class="flc label-{L.lc[j]}"></span>{/if}
            </button>
          {:else}
            <span class="fthumb ghost" aria-hidden="true"></span>
          {/if}
        {/each}
      </div>
    {/if}
  {:else}
    <footer class="mbar">
      <button class="mb danger" onclick={() => store.reject()}><Icon name={inReject ? 'restore' : 'reject'} size={22} /><span>{inReject ? 'Restore' : 'Reject'}</span></button>
      <button class="mb" class:on={(L.r[i] ?? 0) > 0} onclick={() => (sheet = sheet === 'rate' ? null : 'rate')}>
        <Icon name="star" size={22} fill={(L.r[i] ?? 0) > 0} /><span>{(L.r[i] ?? 0) > 0 ? `${L.r[i]}★` : 'Rate'}</span>
      </button>
      <button class="mb" class:pick={L.p[i] === 1} onclick={() => store.togglePick(1)}><Icon name="flag" size={22} fill={L.p[i] === 1} /><span>Pick</span></button>
      <button class="mb" onclick={() => isRaw && store.setViewer('source', useRaw ? 'jpeg' : 'raw')} disabled={!isRaw}>
        <span class="srcbadge" class:raw={useRaw}>{useRaw ? 'RAW' : fileLabel === 'JPEG' ? 'JPG' : fileLabel}</span><span>Source</span>
      </button>
      <button class="mb" class:on={sheet === 'tools'} onclick={() => (sheet = sheet === 'tools' ? null : 'tools')}><Icon name="sliders" size={22} /><span>Tools</span></button>
    </footer>
    {#if sheet}
      <div class="sheet-scrim" onclick={() => (sheet = null)} role="presentation"></div>
      <div class="sheet" class:tall={sheet === 'info'}>
        <div class="grip"></div>
        {#if sheet === 'info'}
          <InfoPanel compact />
        {:else if sheet === 'rate'}
          <div class="mrate">
            {#each [1, 2, 3, 4, 5] as r}
              <button class="star big" class:on={(L.r[i] ?? 0) >= r} onclick={() => { store.rate(r); sheet = null; }}>★</button>
            {/each}
          </div>
          <div class="mlabels">
            {#each LABELS as l}
              <button class="lbl big" class:on={L.lc[i] === l.color} onclick={() => { store.label(l.color); sheet = null; }} aria-label={l.name}><span class="label-dot label-{l.color}"></span></button>
            {/each}
          </div>
          <button class="btn" onclick={() => { store.rate(0); sheet = null; }}>Clear rating</button>
        {:else if sheet === 'tools'}
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
          </div>
          <div class="evrow">
            <button class="btn" onclick={() => { store.setViewer('ev', +(store.viewer.ev - 1 / 3).toFixed(2)); store.setViewer('evOn', true); }}><Icon name="minus" /></button>
            <span class="tnum">{store.viewer.ev > 0 ? '+' : ''}{store.viewer.ev.toFixed(2)} EV</span>
            <button class="btn" onclick={() => { store.setViewer('ev', +(store.viewer.ev + 1 / 3).toFixed(2)); store.setViewer('evOn', true); }}><Icon name="plus" /></button>
            <button class="btn ghost" onclick={() => store.setViewer('ev', 0)}>Reset</button>
          </div>
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
    color: #e8edf2;
    --text: #e8edf2;
    --text-2: #aab6c2;
    --text-3: #8396a8;
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
    background: #141a20;
    border-bottom: 1px solid #222b35;
  }
  .title {
    min-width: 0;
    flex: 0 1 18rem;
  }
  .fname {
    font-weight: 700;
  }
  .sub {
    font-size: var(--fs-sm);
  }
  .tools {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.375rem;
    justify-content: center;
  }
  .tools .seg > button {
    height: 1.75rem;
    font-size: var(--fs-sm);
  }
  .tools sup {
    font-size: 0.625rem;
    margin-left: -0.125rem;
  }
  .vsep {
    width: 1px;
    height: 1.25rem;
    background: #2e3742;
    margin: 0 0.25rem;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    margin-left: auto;
  }
  .loupe :global(.btn) {
    --surface: #1c242c;
    --surface-2: #26303a;
    --border-strong: #3a4a5a;
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
    background: rgba(14, 18, 22, 0.75);
    backdrop-filter: blur(6px);
    color: #e8edf2;
    border: 1px solid rgba(255, 255, 255, 0.08);
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
    background: rgba(250, 97, 97, 0.9);
    color: #fff;
    font-weight: 700;
    padding: 0.25rem 0.75rem;
    border-radius: 1rem;
    font-size: var(--fs-sm);
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    width: 2.75rem;
    height: 4.5rem;
    border: 0;
    border-radius: var(--r);
    background: rgba(0, 0, 0, 0.25);
    color: #fff;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s;
    display: grid;
    place-items: center;
  }
  .stage:hover .nav:not(:disabled) {
    opacity: 0.8;
  }
  .nav:hover {
    background: rgba(0, 0, 0, 0.45);
  }
  .nav.prev {
    left: 0.5rem;
  }
  .nav.next {
    right: 0.5rem;
  }
  .side {
    width: var(--panel-w);
    flex: none;
    overflow: auto;
    background: var(--surface);
    border-left: 1px solid var(--border);
    color: var(--text);
  }
  :global([data-theme='light']) .side {
    --text: #131e29;
    --text-2: #475e75;
    --text-3: #556b82;
  }
  .bottom {
    height: 2.75rem;
    flex: none;
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0 0.75rem;
    background: #141a20;
    border-top: 1px solid #222b35;
  }
  .rate {
    display: flex;
  }
  .star {
    border: 0;
    background: none;
    color: #4a5866;
    font-size: 1.125rem;
    cursor: pointer;
    padding: 0 0.125rem;
    line-height: 1;
  }
  .star.on {
    color: var(--star);
  }
  .star:hover {
    color: #ffd97a;
  }
  .labels {
    display: flex;
    gap: 0.25rem;
  }
  .lbl {
    width: 1.5rem;
    height: 1.5rem;
    border-radius: 50%;
    border: 1px solid transparent;
    background: none;
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .lbl.on {
    border-color: #fff;
  }
  .lbl .label-dot {
    width: 0.875rem;
    height: 0.875rem;
  }
  .btn.pick {
    color: var(--pick);
    border-color: var(--pick);
  }
  .exif {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-sm);
    text-align: right;
  }
  .film {
    height: 4.75rem;
    flex: none;
    display: flex;
    gap: 0.25rem;
    padding: 0.375rem 0.5rem;
    background: #10151a;
    border-top: 1px solid #1d242b;
    overflow: hidden;
    justify-content: center;
  }
  .fthumb {
    height: 100%;
    aspect-ratio: 3 / 2;
    border: 2px solid transparent;
    border-radius: 0.25rem;
    padding: 0;
    background: #1d232a;
    cursor: pointer;
    position: relative;
    overflow: hidden;
    flex: none;
    opacity: 0.7;
  }
  .fthumb.cur {
    border-color: var(--accent);
    opacity: 1;
  }
  .fthumb.ghost {
    visibility: hidden;
  }
  .fthumb:hover {
    opacity: 1;
  }
  .fthumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .fs {
    position: absolute;
    left: 0.125rem;
    bottom: 0;
    font-size: 0.5625rem;
    color: var(--star);
    text-shadow: 0 1px 2px #000;
  }
  .flc {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 3px;
  }

  /* ---------- mobile ---------- */
  .mbar {
    flex: none;
    display: flex;
    justify-content: space-around;
    padding: 0.25rem 0.25rem calc(0.25rem + env(safe-area-inset-bottom));
    background: #141a20;
    border-top: 1px solid #222b35;
  }
  .mb {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.125rem;
    padding: 0.375rem 0;
    border: 0;
    background: none;
    color: #c9d3dc;
    font-size: 0.6875rem;
    font-weight: 600;
    border-radius: var(--r);
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
    border-radius: 0.25rem;
    border: 1.5px solid currentColor;
    display: grid;
    place-items: center;
    font-size: 0.6875rem;
    font-weight: 800;
  }
  .srcbadge.raw {
    color: #ffb84d;
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
  }
  .sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 56;
    background: var(--surface);
    color: var(--text);
    border-radius: 1rem 1rem 0 0;
    box-shadow: var(--shadow-2);
    padding: 0.5rem 1rem calc(1rem + env(safe-area-inset-bottom));
    max-height: 70vh;
    overflow: auto;
    animation: up 0.18s var(--ease);
  }
  .sheet.tall {
    max-height: 82vh;
    padding: 0.5rem 0 calc(1rem + env(safe-area-inset-bottom));
  }
  :global([data-theme='light']) .sheet {
    --text: #131e29;
    --text-2: #475e75;
    --text-3: #556b82;
  }
  @keyframes up {
    from {
      transform: translateY(100%);
    }
  }
  .grip {
    width: 2.5rem;
    height: 0.25rem;
    border-radius: 0.25rem;
    background: var(--border-strong);
    margin: 0 auto 0.75rem;
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
    border: 1px solid var(--border-strong);
  }
  .lbl.big .label-dot {
    width: 1.5rem;
    height: 1.5rem;
  }
  .lbl.big.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent);
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
    gap: 0.25rem;
    padding: 0.625rem 0.25rem;
    border-radius: var(--r-lg);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    font-size: 0.6875rem;
    font-weight: 600;
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
    margin-top: 1rem;
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
