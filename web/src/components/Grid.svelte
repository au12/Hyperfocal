<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { urls } from '../lib/api';
  import { aspectOf, justify, rowAt } from '../lib/layout';
  import { gridCtl } from '../lib/ctl';
  import { thumb, thumbs } from '../lib/thumbs';
  import { bump, reduced } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';
  import { cull, ISSUES } from '../lib/cull.svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import Icon from './Icon.svelte';

  let scroller: HTMLDivElement | undefined = $state();
  let width = $state(0);
  let viewH = $state(0);
  let scrollTop = $state(0);
  const PAD = 12;

  const gap = $derived(store.mobile ? 3 : 6);
  const target = $derived(store.mobile ? Math.round(store.settings.rowHeight * 0.62) : store.settings.rowHeight);

  const aspects = $derived.by(() => {
    void store.rev;
    const L = store.list;
    const a = new Float32Array(L.id.length);
    for (let i = 0; i < a.length; i++) a[i] = aspectOf(L.w[i], L.h[i], L.o[i]);
    return a;
  });

  const pad = $derived(store.mobile ? 4 : PAD);
  const layout = $derived(justify(aspects, Math.max(0, width - pad * 2), target, gap));

  // Tiles glide to their new places after a change that reflows the grid (a reject, a
  // restore): `store.reflow` is bumped, and for a moment positions are transitioned.
  let gliding = $state(false);
  let glideTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (!store.reflow) return;
    gliding = true;
    clearTimeout(glideTimer);
    glideTimer = setTimeout(() => (gliding = false), 520);
  });

  const OVER = 1.2;
  const visible = $derived.by(() => {
    const rows = layout.rows;
    if (!rows.length) return [];
    const over = viewH * OVER;
    const a = rowAt(rows, Math.max(0, scrollTop - pad - over));
    const out = [];
    for (let r = a; r < rows.length; r++) {
      if (rows[r].top > scrollTop - pad + viewH + over) break;
      out.push(rows[r]);
    }
    return out;
  });

  /** The tiles to render (the visible rows, flattened), keyed by photo: a tile keeps its
   * element when the grid reflows, so it can glide to its new place. */
  const tiles = $derived.by(() => {
    const out: { i: number; x: number; y: number; w: number; h: number }[] = [];
    for (const row of visible) {
      for (let k = 0; k < row.end - row.start; k++) out.push({ i: row.start + k, x: pad + row.x[k], y: pad + row.top, w: row.w[k], h: row.height });
    }
    return out;
  });

  // ------------------------------------------------------------ thumbnail order
  // Thumbnails load nearest the viewport first: the rows on screen (from the middle
  // out), then the ones scrolling into view, then the ones behind. While scrolling
  // very fast nothing new starts; once it settles, the next screens ahead are fetched.

  let dir = 1;
  let lastTop = 0;
  let lastT = 0;

  function rank(i: number): number {
    const r = layout.rows[layout.rowOf[i]];
    if (!r || !viewH) return Infinity;
    const top = r.top + pad;
    const bot = top + r.height;
    const vt = scrollTop;
    const vb = scrollTop + viewH;
    if (bot > vt && top < vb) return Math.abs((top + bot) / 2 - (vt + vb) / 2) / viewH / 2;
    const below = top >= vb;
    const gap = below ? top - vb : vt - bot;
    return 1 + (gap / viewH) * (below === dir > 0 ? 1 : 2.5);
  }

  function onScrollSpeed(top: number) {
    const now = performance.now();
    const dt = now - lastT;
    if (top !== lastTop) dir = top > lastTop ? 1 : -1;
    // Faster than about three screens a second (a fling, the scroll bar dragged).
    if (dt > 0 && dt < 200 && viewH && Math.abs(top - lastTop) / dt > (3 * viewH) / 1000) thumbs.hold(60);
    lastTop = top;
    lastT = now;
  }

  // The next two screens ahead of the rendered rows, once scrolling settles.
  $effect(() => {
    const rows = layout.rows;
    const st = scrollTop;
    const h = viewH;
    const d = dir;
    const L = store.list;
    void store.rev;
    if (!rows.length || !h) return;
    const t = setTimeout(() => {
      const out: { url: string; i: number }[] = [];
      const from = d > 0 ? st - pad + h * (1 + OVER) : st - pad - h * (OVER + 2);
      const to = from + h * 2;
      for (let r = rowAt(rows, Math.max(0, from)); r < rows.length && rows[r].top <= to; r++) {
        for (let i = rows[r].start; i < rows[r].end; i++) out.push({ url: urls.thumb(L.id[i], L.v[i], store.thumbRev[L.id[i]]), i });
      }
      thumbs.ahead(out);
    }, 150);
    return () => clearTimeout(t);
  });

  onMount(() => {
    thumbs.rank = rank;
    const ro = new ResizeObserver(() => {
      if (!scroller) return;
      width = scroller.clientWidth;
      viewH = scroller.clientHeight;
    });
    if (scroller) ro.observe(scroller);
    // While a hold-and-slide selection is on, the finger selects instead of scrolling.
    // This has to be a non-passive listener that exists before the touch starts.
    const noPan = (e: TouchEvent) => {
      if (press?.held && e.cancelable) e.preventDefault();
    };
    scroller?.addEventListener('touchmove', noPan, { passive: false });
    gridCtl.rowNeighbor = (i: number, d: number) => {
      const r = layout.rowOf[i] ?? 0;
      const row = layout.rows[r];
      const next = layout.rows[r + d];
      if (!row || !next) return null;
      const cx = row.x[i - row.start] + row.w[i - row.start] / 2;
      let best = next.start;
      let bd = Infinity;
      for (let k = next.start; k < next.end; k++) {
        const c = next.x[k - next.start] + next.w[k - next.start] / 2;
        if (Math.abs(c - cx) < bd) {
          bd = Math.abs(c - cx);
          best = k;
        }
      }
      return best;
    };
    gridCtl.vertical = (d: number) => {
      const to = gridCtl.rowNeighbor(store.cursor, d);
      if (to != null) store.moveCursor(to);
    };
    gridCtl.pageTarget = (d: number) => {
      if (!scroller) return null;
      const rows = layout.rows;
      const from = rows[layout.rowOf[store.cursor] ?? 0];
      if (!from) return null;
      // The photo a screenful above/below the cursor, in the same column (nearest centre).
      const step = d * Math.max(target, viewH - target);
      const r = Math.max(0, Math.min(rows.length - 1, rowAt(rows, Math.max(0, from.top + from.height / 2 + step))));
      scroller.scrollBy({ top: step });
      if (r === layout.rowOf[store.cursor]) return d > 0 ? store.list.id.length - 1 : 0;
      const k = store.cursor - from.start;
      const cx = from.x[k] + from.w[k] / 2;
      const row = rows[r];
      let best = row.start;
      let bd = Infinity;
      for (let j = row.start; j < row.end; j++) {
        const dist = Math.abs(row.x[j - row.start] + row.w[j - row.start] / 2 - cx);
        if (dist < bd) {
          bd = dist;
          best = j;
        }
      }
      return best;
    };
    gridCtl.page = (d: number) => {
      const to = gridCtl.pageTarget(d);
      if (to != null) store.moveCursor(to);
    };
    gridCtl.reveal = reveal;
    return () => {
      ro.disconnect();
      scroller?.removeEventListener('touchmove', noPan);
      endPress();
      thumbs.ahead([]);
      if (thumbs.rank === rank) thumbs.rank = (i) => i;
    };
  });

  function reveal(i: number) {
    if (!scroller) return;
    const r = layout.rows[layout.rowOf[i]];
    if (!r) return;
    const top = r.top + pad;
    if (top < scroller.scrollTop) scroller.scrollTop = top - 8;
    else if (top + r.height > scroller.scrollTop + viewH) scroller.scrollTop = top + r.height - viewH + 8;
  }

  // Keep the keyboard cursor in view, and land on the photo the single view was showing.
  $effect(() => {
    const c = store.cursor;
    void layout;
    if (!store.loupe) queueMicrotask(() => reveal(c));
  });

  // Scroll to top on folder change.
  let lastFolder: number | null = null;
  $effect(() => {
    const f = store.folderId;
    void store.query;
    if (f !== lastFolder && scroller) {
      scroller.scrollTop = 0;
      scrollTop = 0;
    }
    lastFolder = f;
  });

  // ------------------------------------------------------------ touch selection
  // Hold a photo to select it. Keep holding and slide: everything from that photo to the
  // one under the finger is selected, in grid order (sliding back shrinks it), and the
  // grid scrolls near the top and bottom edges. Starting on a selected photo deselects
  // instead. A new hold keeps what was already selected, so separate ranges add up.
  // When exactly one photo is selected, holding another and letting go without sliding
  // also selects everything between the two; sliding instead starts a separate range.
  // Taps still add or remove single photos.

  const HOLD_MS = 450;
  const SLOP = 10; // px the finger may wander before the hold counts as a scroll
  const TOUCH_EDGE = 52;

  type Press = {
    pointer: number;
    i: number;
    sx: number;
    sy: number;
    cx: number;
    cy: number;
    timer: ReturnType<typeof setTimeout>;
    held: boolean;
    anchor: number;
    to: number;
    base: Set<number>;
    remove: boolean;
    /** The one photo selected before this hold: letting go without sliding fills the
     * gap from it. Cleared as soon as the finger slides onto another photo. */
    fill: number | null;
    raf: number;
  };
  let press: Press | null = null;
  let longPressed = false;

  function onDown(i: number, e: PointerEvent) {
    if (e.pointerType === 'mouse') return;
    longPressed = false;
    // A second finger (a pinch) ends a hold that hasn't fired yet.
    if (press) {
      if (!press.held) endPress();
      return;
    }
    press = {
      pointer: e.pointerId,
      i,
      sx: e.clientX,
      sy: e.clientY,
      cx: e.clientX,
      cy: e.clientY,
      timer: setTimeout(onHold, HOLD_MS),
      held: false,
      anchor: i,
      to: i,
      base: new Set(),
      remove: false,
      fill: null,
      raf: 0,
    };
    window.addEventListener('pointermove', onPressMove);
    window.addEventListener('pointerup', onPressUp);
    window.addEventListener('pointercancel', onPressUp);
    window.addEventListener('blur', endPress);
  }

  function onHold() {
    const p = press;
    if (!p) return;
    const L = store.list;
    const id = L.id[p.i];
    if (id == null) return endPress();
    p.held = true;
    longPressed = true;
    const sel = store.selected;
    const only = sel.size === 1 ? [...sel][0] : null;
    const from = only != null && only !== id ? store.index.get(only) : undefined;
    p.fill = from ?? null;
    p.anchor = p.i;
    p.base = new Set(sel);
    p.remove = sel.has(id);
    p.to = p.i;
    navigator.vibrate?.(15);
    store.pressRange(p.anchor, p.to, p.base, p.remove);
    store.cursor = p.i;
    document.body.classList.add('no-select');
  }

  function onPressMove(e: PointerEvent) {
    const p = press;
    if (!p || e.pointerId !== p.pointer) return;
    p.cx = e.clientX;
    p.cy = e.clientY;
    if (!p.held) {
      // Moved before the hold fired: it's a scroll, not a selection.
      if (Math.hypot(p.cx - p.sx, p.cy - p.sy) > SLOP) endPress();
      return;
    }
    updatePress();
    pressScroll();
  }

  function onPressUp(e: PointerEvent) {
    const p = press;
    if (!p || e.pointerId !== p.pointer) return;
    // Held a second photo and let go without sliding: fill the gap from the first.
    if (e.type === 'pointerup' && p.held && p.fill != null && p.to === p.i) {
      store.pressRange(p.fill, p.i, new Set(), false);
      navigator.vibrate?.([12, 40, 12]);
    }
    endPress();
  }

  function endPress() {
    window.removeEventListener('pointermove', onPressMove);
    window.removeEventListener('pointerup', onPressUp);
    window.removeEventListener('pointercancel', onPressUp);
    window.removeEventListener('blur', endPress);
    if (press) {
      clearTimeout(press.timer);
      if (press.raf) cancelAnimationFrame(press.raf);
      if (press.held) document.body.classList.remove('no-select');
    }
    press = null;
  }

  /** The photo under (or nearest to) a point in layout coordinates: between photos in a
   * row the one before, between rows the row below, above the grid the first photo,
   * below it the last. */
  function indexAt(x: number, y: number): number | null {
    const rows = layout.rows;
    if (!rows.length) return null;
    if (y < 0) return 0;
    const row = rows[rowAt(rows, y)];
    if (y > row.top + row.height && row === rows[rows.length - 1]) return row.end - 1;
    let k = 0;
    while (k + 1 < row.end - row.start && row.x[k + 1] <= x) k++;
    return row.start + k;
  }

  function updatePress() {
    const p = press;
    if (!p?.held || !scroller) return;
    const at = toLayout(p.cx, p.cy);
    const to = indexAt(at.x, at.y);
    if (to == null || to === p.to) return;
    p.to = to;
    p.fill = null;
    store.pressRange(p.anchor, to, p.base, p.remove);
  }

  /** Scrolls while the finger is near (or past) the top or bottom edge, faster the
   * closer it gets. */
  function pressScroll() {
    const p = press;
    if (!p || !scroller || p.raf) return;
    const tick = () => {
      const q = press;
      if (!q || !scroller) return;
      q.raf = 0;
      const r = scroller.getBoundingClientRect();
      let v = 0;
      if (q.cy < r.top + TOUCH_EDGE) v = -Math.min(TOUCH_EDGE, r.top + TOUCH_EDGE - q.cy);
      else if (q.cy > r.bottom - TOUCH_EDGE) v = Math.min(TOUCH_EDGE, q.cy - (r.bottom - TOUCH_EDGE));
      if (!v) return;
      const before = scroller.scrollTop;
      scroller.scrollTop += v * 0.35;
      if (scroller.scrollTop !== before) {
        scrollTop = scroller.scrollTop;
        updatePress();
        q.raf = requestAnimationFrame(tick);
      }
    };
    p.raf = requestAnimationFrame(tick);
  }

  function onClick(i: number, e: MouseEvent) {
    if (longPressed) {
      longPressed = false;
      return;
    }
    if (store.mobile) {
      if (store.selected.size > 0) store.select(i, 'toggle');
      else store.openLoupe(i);
      return;
    }
    // As in Finder / Explorer: Shift selects a range from the last clicked photo,
    // Ctrl/Cmd adds or removes one, a plain click selects just that one.
    if (e.shiftKey) store.extendTo(i);
    else if (e.ctrlKey || e.metaKey) store.select(i, 'toggle', true);
    else store.clickSelect(i);
  }

  // ------------------------------------------------------------ rubber band
  // Dragging with the mouse (from a photo or the gaps between them) draws a selection
  // rectangle. Plain: just the photos it touches. Shift: added to the selection.
  // Ctrl/Cmd: toggled. Near the top or bottom edge the grid scrolls.

  type Drag = {
    x0: number;
    y0: number;
    cx: number;
    cy: number;
    sx: number;
    sy: number;
    mode: 'replace' | 'add' | 'toggle';
    base: Set<number>;
    active: boolean;
    raf: number;
    last: number[];
  };
  let drag: Drag | null = null;
  let band = $state<{ x: number; y: number; w: number; h: number } | null>(null);
  let swallowClick = false;
  const EDGE = 48;

  /** Client coordinates to layout coordinates (row.x / row.top space). */
  function toLayout(cx: number, cy: number) {
    const r = scroller!.getBoundingClientRect();
    return { x: cx - r.left - pad + scroller!.scrollLeft, y: cy - r.top - pad + scroller!.scrollTop };
  }

  function hitTest(x0: number, y0: number, x1: number, y1: number): number[] {
    const rows = layout.rows;
    const out: number[] = [];
    if (!rows.length) return out;
    const [xa, xb] = x0 < x1 ? [x0, x1] : [x1, x0];
    const [ya, yb] = y0 < y1 ? [y0, y1] : [y1, y0];
    for (let r = rowAt(rows, ya); r < rows.length && rows[r].top <= yb; r++) {
      const row = rows[r];
      if (row.top + row.height < ya) continue;
      for (let k = 0; k < row.end - row.start; k++) {
        if (row.x[k] <= xb && row.x[k] + row.w[k] >= xa) out.push(row.start + k);
      }
    }
    return out;
  }

  // ------------------------------------------------------------ arranging an album
  // In an album shown in its own order, a photo dragged by the mouse (with the rest of
  // the selection, when it is selected) lifts out as a small stack; a marker shows where
  // it will land, and the grid glides into the new order on release.
  interface Arrange {
    sx: number;
    sy: number;
    cx: number;
    cy: number;
    id: number;
    ids: number[];
    active: boolean;
    raf: number;
    ghost: HTMLElement | null;
    before: number | null | undefined;
  }
  let arrange: Arrange | null = null;
  const lifted = new SvelteSet<number>();
  let marker = $state<{ x: number; y: number; h: number } | null>(null);

  function startArrange(e: PointerEvent, id: number) {
    arrange = { sx: e.clientX, sy: e.clientY, cx: e.clientX, cy: e.clientY, id, ids: [], active: false, raf: 0, ghost: null, before: undefined };
    window.addEventListener('pointermove', onArrangeMove);
    window.addEventListener('pointerup', onArrangeUp);
    window.addEventListener('pointercancel', cancelArrange);
    window.addEventListener('blur', cancelArrange);
  }

  function makeGhost(ids: number[]): HTMLElement {
    const g = document.createElement('div');
    g.className = 'arrange-ghost';
    ids.slice(0, 3).forEach((id, k) => {
      const i = store.index.get(id);
      if (i == null) return;
      const img = document.createElement('img');
      img.src = urls.thumb(id, store.list.v[i], store.thumbRev[id]);
      img.alt = '';
      img.style.setProperty('--k', String(k));
      img.style.zIndex = String(3 - k);
      g.appendChild(img);
    });
    if (ids.length > 1) {
      const n = document.createElement('span');
      n.className = 'n';
      n.textContent = ids.length.toLocaleString();
      g.appendChild(n);
    }
    document.body.appendChild(g);
    return g;
  }

  /** Where a photo dropped at a point would land: before which photo (null: at the
   * end), and the marker between the tiles. */
  function slotAt(cx: number, cy: number): { before: number | null; x: number; y: number; h: number } | null {
    const rows = layout.rows;
    if (!rows.length || !scroller) return null;
    const p = toLayout(cx, cy);
    const row = rows[Math.max(0, Math.min(rows.length - 1, rowAt(rows, p.y)))];
    const n = row.end - row.start;
    let k = n;
    for (let j = 0; j < n; j++) {
      if (p.x < row.x[j] + row.w[j] / 2) {
        k = j;
        break;
      }
    }
    const idx = row.start + k;
    const before = idx < store.list.id.length ? store.list.id[idx] : null;
    const x = k < n ? row.x[k] - gap / 2 : row.x[n - 1] + row.w[n - 1] + gap / 2;
    return { before, x, y: row.top, h: row.height };
  }

  function onArrangeMove(e: PointerEvent) {
    const a = arrange;
    if (!a) return;
    a.cx = e.clientX;
    a.cy = e.clientY;
    if (!a.active) {
      if (Math.hypot(e.clientX - a.sx, e.clientY - a.sy) < 6) return;
      a.active = true;
      // The selection moves together when the photo is part of it.
      a.ids = store.selected.has(a.id) ? store.list.id.filter((id) => store.selected.has(id)) : [a.id];
      for (const id of a.ids) lifted.add(id);
      a.ghost = makeGhost(a.ids);
      document.body.classList.add('arranging');
    }
    if (a.ghost) a.ghost.style.transform = `translate(${e.clientX + 14}px, ${e.clientY + 10}px)`;
    updateSlot();
    arrangeScroll();
  }

  function updateSlot() {
    const a = arrange;
    if (!a?.active) return;
    const s = slotAt(a.cx, a.cy);
    a.before = s?.before;
    marker = s ? { x: s.x, y: s.y, h: s.h } : null;
  }

  function arrangeScroll() {
    const a = arrange;
    if (!a || !scroller || a.raf) return;
    const tick = () => {
      if (!arrange || !scroller) return;
      arrange.raf = 0;
      const r = scroller.getBoundingClientRect();
      let v = 0;
      if (arrange.cy < r.top + EDGE) v = -Math.min(EDGE, r.top + EDGE - arrange.cy);
      else if (arrange.cy > r.bottom - EDGE) v = Math.min(EDGE, arrange.cy - (r.bottom - EDGE));
      if (!v) return;
      const before = scroller.scrollTop;
      scroller.scrollTop += v * 0.6;
      if (scroller.scrollTop !== before) {
        scrollTop = scroller.scrollTop;
        updateSlot();
        arrange.raf = requestAnimationFrame(tick);
      }
    };
    a.raf = requestAnimationFrame(tick);
  }

  function endArrange(): Arrange | null {
    const a = arrange;
    arrange = null;
    window.removeEventListener('pointermove', onArrangeMove);
    window.removeEventListener('pointerup', onArrangeUp);
    window.removeEventListener('pointercancel', cancelArrange);
    window.removeEventListener('blur', cancelArrange);
    document.body.classList.remove('arranging');
    if (a?.raf) cancelAnimationFrame(a.raf);
    marker = null;
    lifted.clear();
    return a;
  }

  function onArrangeUp() {
    const a = endArrange();
    if (!a?.active) return;
    swallowClick = true;
    const g = a.ghost;
    if (a.before !== undefined) {
      // A drop just before one of the photos moved means "before the next one that stays".
      let before = a.before;
      const moving = new Set(a.ids);
      if (before != null && moving.has(before)) {
        let i = store.index.get(before) ?? 0;
        while (i < store.list.id.length && moving.has(store.list.id[i])) i++;
        before = i < store.list.id.length ? store.list.id[i] : null;
      }
      albums.move(a.ids, before);
    }
    if (g) {
      // The stack shrinks into place as the tiles glide.
      const anim = reduced() ? null : g.animate([{ opacity: 1 }, { opacity: 0, scale: '0.6' }], { duration: 180, easing: 'ease-in', fill: 'forwards' });
      if (anim) anim.finished.then(() => g.remove(), () => g.remove());
      else g.remove();
    }
  }

  function cancelArrange() {
    endArrange()?.ghost?.remove();
  }

  function onGridDown(e: PointerEvent) {
    swallowClick = false;
    if (store.mobile || e.pointerType !== 'mouse' || e.button !== 0 || !scroller) return;
    const r = scroller.getBoundingClientRect();
    if (e.clientX >= r.left + scroller.clientWidth) return; // the scrollbar
    // No text selection while shift-clicking or dragging; leave any text field so the
    // arrow keys drive the grid again.
    e.preventDefault();
    const ae = document.activeElement as HTMLElement | null;
    if (ae && (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.isContentEditable)) ae.blur();
    // An album in its own order: a photo pressed (without Shift or Ctrl) can be dragged
    // into place; the gaps still start a selection band.
    const tileEl = (e.target as HTMLElement).closest<HTMLElement>('.tile');
    if (albums.arranging && tileEl && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
      startArrange(e, Number(tileEl.dataset.id));
      return;
    }
    const p = toLayout(e.clientX, e.clientY);
    const mode = e.ctrlKey || e.metaKey ? 'toggle' : e.shiftKey ? 'add' : 'replace';
    drag = { x0: p.x, y0: p.y, cx: e.clientX, cy: e.clientY, sx: e.clientX, sy: e.clientY, mode, base: new Set(), active: false, raf: 0, last: [] };
    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragUp);
    window.addEventListener('pointercancel', onDragUp);
    window.addEventListener('blur', cancelDrag);
  }

  function onDragMove(e: PointerEvent) {
    if (!drag) return;
    drag.cx = e.clientX;
    drag.cy = e.clientY;
    if (!drag.active) {
      if (Math.hypot(e.clientX - drag.sx, e.clientY - drag.sy) < 5) return;
      drag.active = true;
      drag.base = drag.mode === 'replace' ? new Set() : store.marqueeBase();
      document.body.classList.add('no-select');
    }
    updateBand();
    autoScroll();
  }

  function updateBand() {
    if (!drag || !drag.active || !scroller) return;
    const p = toLayout(drag.cx, drag.cy);
    const maxX = Math.max(0, width - pad * 2);
    const x1 = Math.max(-pad, Math.min(maxX + pad, p.x));
    const y1 = Math.max(-pad, Math.min(layout.height + pad, p.y));
    band = { x: Math.min(drag.x0, x1), y: Math.min(drag.y0, y1), w: Math.abs(x1 - drag.x0), h: Math.abs(y1 - drag.y0) };
    const hits = hitTest(drag.x0, drag.y0, x1, y1);
    const prev = drag.last;
    const same = hits.length === prev.length && hits.every((h, j) => h === prev[j]);
    if (!same) {
      drag.last = hits;
      store.marquee(hits, drag.mode, drag.base);
    }
  }

  function autoScroll() {
    if (!drag || !scroller || drag.raf) return;
    const tick = () => {
      if (!drag || !scroller) return;
      drag.raf = 0;
      const r = scroller.getBoundingClientRect();
      let v = 0;
      if (drag.cy < r.top + EDGE) v = -Math.min(EDGE, r.top + EDGE - drag.cy);
      else if (drag.cy > r.bottom - EDGE) v = Math.min(EDGE, drag.cy - (r.bottom - EDGE));
      if (!v) return;
      const before = scroller.scrollTop;
      scroller.scrollTop += v * 0.6;
      if (scroller.scrollTop !== before) {
        scrollTop = scroller.scrollTop;
        updateBand();
        drag.raf = requestAnimationFrame(tick);
      }
    };
    drag.raf = requestAnimationFrame(tick);
  }

  function endDrag() {
    window.removeEventListener('pointermove', onDragMove);
    window.removeEventListener('pointerup', onDragUp);
    window.removeEventListener('pointercancel', onDragUp);
    window.removeEventListener('blur', cancelDrag);
    document.body.classList.remove('no-select');
    if (drag?.raf) cancelAnimationFrame(drag.raf);
    band = null;
  }

  function onDragUp() {
    const d = drag;
    endDrag();
    drag = null;
    if (!d?.active) return;
    // The click that follows the drag must not reselect the photo under the pointer.
    swallowClick = true;
    const hits = d.last;
    // The cursor goes to the photo nearest where the band ended (it is on screen).
    const end = toLayout(d.cx, d.cy);
    const at = hits.length ? (end.y >= d.y0 ? hits[hits.length - 1] : hits[0]) : null;
    store.marqueeEnd(at != null && store.selected.has(store.list.id[at]) ? at : null);
  }

  function cancelDrag() {
    endDrag();
    drag = null;
  }

  function onGridClickCapture(e: MouseEvent) {
    if (swallowClick) {
      swallowClick = false;
      e.stopPropagation();
      e.preventDefault();
    }
  }

  /** A click on the gaps between photos clears the selection. */
  function onGridClick(e: MouseEvent) {
    if (store.mobile || (e.target as HTMLElement).closest('.tile')) return;
    if (e.shiftKey || e.ctrlKey || e.metaKey) return;
    if (store.selected.size) store.clearSelection();
  }

  function stars(n: number) {
    return n > 0 ? '★'.repeat(n) : '';
  }

  /** Is tile `i` the first of an unfolded stack (where its pill sits)? */
  function firstOfOpen(i: number, key: number): boolean {
    return (store.list.sk?.[i - 1] ?? 0) !== key;
  }

  function issueTitle(bits: number): string {
    return ISSUES.filter((x) => bits & x.bit).map((x) => x.label).join(' · ');
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="scroller"
  bind:this={scroller}
  onscroll={() => {
    scrollTop = scroller?.scrollTop ?? 0;
    onScrollSpeed(scrollTop);
    updateBand();
    updatePress();
  }}
  onpointerdown={onGridDown}
  onclickcapture={onGridClickCapture}
  onclick={onGridClick}
  tabindex="-1"
  role="grid"
>
  {#if store.list.id.length === 0 && !store.loading}
    <div class="empty">
      <Icon name="image" size={40} />
      {#if store.listError}
        <p>{store.listError}</p>
      {:else if store.query}
        <p>No photos match “{store.query}”.</p>
      {:else}
        <p>No photos here{store.recursive ? '' : ' — try “Sub folders”'}.</p>
      {/if}
    </div>
  {/if}
  <div class="spacer" class:gliding style:height={`${layout.height + pad * 2}px`}>
    {#each tiles as t (store.list.id[t.i])}
      {@const i = t.i}
      {@const id = store.list.id[i]}
      {@const _r = store.rev}
      {@const cq = cull.on() ? (store.list.cq?.[i] ?? -1) : -1}
      {@const sk = store.list.sk?.[i] ?? 0}
      {@const sn = store.list.sn?.[i] ?? 0}
      {@const unfolded = sk !== 0 && cull.open.has(sk)}
      {@const pill = sk !== 0 && (!unfolded || firstOfOpen(i, sk))}
      {@const best = unfolded && store.list.sb?.[i] === 1}
      {@const narrow = t.w < 150}
      <!-- Keyboard culling is handled by the global key map (arrows, ratings, P/X…). -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <div
        class="tile"
        class:cursor={i === store.cursor}
        class:sel={store.selected.has(id)}
        class:rejected={store.folders.get(store.list.f[i])?.isReject}
        class:leaving={store.leaving.has(id)}
        class:lifted={lifted.has(id)}
        class:arrangeable={albums.arranging}
        class:stacked={sk !== 0 && !unfolded}
        class:unfolded
        class:best
        class:poor={cq > 0}
        class:narrow
        class:pilled={pill || best}
        style:transform={`translate3d(${t.x}px, ${t.y}px, 0)`}
        style:width={`${t.w}px`}
        style:height={`${t.h}px`}
        onclick={(e) => onClick(i, e)}
        ondblclick={() => !store.mobile && store.openLoupe(i)}
        onpointerdown={(e) => onDown(i, e)}
        oncontextmenu={(e) => store.mobile && e.preventDefault()}
        role="gridcell"
        tabindex="-1"
        aria-selected={store.selected.has(id) || (store.selected.size === 0 && i === store.cursor)}
        title={store.list.n[i]}
        data-id={id}
      >
        <img use:thumb={{ url: urls.thumb(id, store.list.v[i], store.thumbRev[id]), i }} alt="" decoding="async" draggable="false" onload={(e) => e.currentTarget.classList.add('ld')} />
        <span class="sk" aria-hidden="true"></span>
        {#if store.list.lc[i] && _r >= 0}<span class="lc label-{store.list.lc[i]}"></span>{/if}
        {#if sk !== 0 && !unfolded}<span class="sheets" aria-hidden="true"></span>{/if}
        {#if unfolded}<span class="rail" class:start={firstOfOpen(i, sk)} class:end={(store.list.sk?.[i + 1] ?? 0) !== sk}></span>{/if}
        <div class="corner">
          {#if cq > 0}
            <span class="issues" title={issueTitle(cq)}>
              {#each ISSUES as is (is.key)}
                {#if cq & is.bit}<span class="issue {is.key}"><Icon name={is.icon} size={12} stroke={2.2} /></span>{/if}
              {/each}
            </span>
          {/if}
          {#if store.list.p[i] === 1}<span class="flag"><Icon name="flag" size={14} fill /></span>{/if}
        </div>
        {#if store.selected.has(id)}<span class="check"><Icon name="check" size={13} stroke={3} /></span>{/if}
        {#if pill || best}
          <div class="pills" class:shifted={store.selected.has(id)}>
            {#if pill}
              <button
                class="stack-pill"
                class:open={unfolded}
                onclick={(e) => { e.stopPropagation(); cull.toggleStack(sk); }}
                ondblclick={(e) => e.stopPropagation()}
                onpointerdown={(e) => e.stopPropagation()}
                title={unfolded ? `Fold the series back to its best frame${store.keyHint('stack')}` : `A series of ${sn}: this is its best frame. Show them all${store.keyHint('stack')}`}
              >
                <Icon name="stack" size={12} stroke={2.2} /><span class="tnum">{sn}</span>{#if unfolded && !narrow}<Icon name="chevron-left" size={11} stroke={2.4} />{/if}
              </button>
            {/if}
            {#if best}<span class="best-pill" title="The best frame of the series">Best</span>{/if}
          </div>
        {/if}
        <div class="meta">
          {#if store.list.r[i] > 0}<span class="stars" use:bump={store.list.r[i]}>{stars(store.list.r[i])}</span>{:else if store.list.r[i] < 0}<span class="rej-r">✕</span>{/if}
          <span class="grow"></span>
          {#if store.settings.showNames}<span class="name ellipsis">{store.list.n[i]}</span>{/if}
          <span class="ext">{store.list.x[i].toUpperCase()}</span>
        </div>
      </div>
    {/each}
    {#if marker}
      <div class="drop-marker" style:transform={`translate(${pad + marker.x - 1.5}px, ${pad + marker.y}px)`} style:height={`${marker.h}px`}></div>
    {/if}
    {#if band}
      <div class="band" style:left={`${pad + band.x}px`} style:top={`${pad + band.y}px`} style:width={`${band.w}px`} style:height={`${band.h}px`}></div>
    {/if}
  </div>
</div>

<style>
  .scroller {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    outline: none;
    contain: strict;
  }
  .spacer {
    position: relative;
    width: 100%;
  }
  .tile {
    position: absolute;
    left: 0;
    top: 0;
    overflow: hidden;
    border-radius: 0.375rem;
    background: var(--placeholder);
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
    -webkit-touch-callout: none;
    contain: layout paint;
    transition:
      scale 0.22s var(--ease-out),
      opacity 0.22s var(--ease-out),
      filter 0.22s;
  }
  /* A reflow (a reject, a restore): tiles glide to their new places. */
  .gliding .tile {
    transition:
      transform 0.42s var(--ease-out),
      width 0.42s var(--ease-out),
      height 0.42s var(--ease-out),
      scale 0.22s var(--ease-out),
      opacity 0.22s var(--ease-out);
  }
  /* Being dragged into a new place in the album. */
  .tile.lifted {
    scale: 0.94;
    opacity: 0.32;
    filter: saturate(0.4);
  }
  .tile.arrangeable {
    cursor: grab;
  }
  /* Where the dragged photos will land: a bar between two tiles that slides from gap to
     gap. */
  .drop-marker {
    position: absolute;
    left: 0;
    top: 0;
    width: 3px;
    border-radius: 3px;
    background: var(--accent);
    box-shadow:
      0 0 0 3px var(--accent-bg-strong),
      0 0 18px 2px color-mix(in srgb, var(--accent) 60%, transparent);
    pointer-events: none;
    z-index: 5;
    transition:
      transform 0.16s var(--ease-out),
      height 0.16s var(--ease-out);
    animation: marker-in 0.22s var(--ease-spring);
  }
  @keyframes marker-in {
    from {
      opacity: 0;
      scale: 1 0.4;
    }
  }
  /* On its way to the reject folder (until the server confirms the move). */
  .tile.leaving {
    scale: 0.82;
    opacity: 0;
    filter: blur(3px) grayscale(1);
    pointer-events: none;
  }
  .tile img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transform: scale(1.04);
    transition:
      opacity 0.32s var(--ease-out),
      transform 0.6s var(--ease-out);
  }
  .tile img:global(.ld) {
    opacity: 1;
    transform: scale(1);
  }
  @media (hover: hover) {
    .tile:hover img:global(.ld) {
      transform: scale(1.035);
    }
  }
  /* Loading: a soft sweep of light, until the thumbnail is in. */
  .sk {
    position: absolute;
    inset: 0;
    overflow: hidden;
    pointer-events: none;
    transition: opacity 0.3s;
  }
  .sk::before {
    content: '';
    position: absolute;
    inset: 0;
    width: 60%;
    background: linear-gradient(100deg, transparent, var(--sheen), transparent);
    animation: sk 1.6s var(--ease-in-out) infinite;
  }
  @keyframes sk {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(260%);
    }
  }
  img:global(.ld) + .sk {
    opacity: 0;
  }
  img:global(.ld) + .sk::before {
    animation: none;
  }
  .tile::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.04);
    transition:
      box-shadow var(--dur) var(--ease-out),
      background-color var(--dur);
  }
  .tile:hover::after {
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.28);
  }
  .tile.cursor::after {
    box-shadow:
      inset 0 0 0 2px var(--accent),
      inset 0 0 0 3.5px rgba(0, 0, 0, 0.45);
  }
  .tile.sel::after {
    box-shadow:
      inset 0 0 0 3px var(--accent-strong),
      inset 0 0 0 4.5px rgba(0, 0, 0, 0.35);
    background: rgba(99, 114, 255, 0.16);
  }
  .tile.rejected img:global(.ld) {
    opacity: 0.45;
    filter: grayscale(0.6);
  }
  .lc {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 3px;
  }
  /* Top right: what may be wrong (culling), then the pick flag. */
  .corner {
    position: absolute;
    top: 0.375rem;
    right: 0.375rem;
    display: flex;
    align-items: center;
    gap: 0.3125rem;
    pointer-events: none;
  }
  .issues {
    display: flex;
    gap: 0.1875rem;
    padding: 0.1875rem;
    border-radius: 99px;
    background: rgba(12, 13, 16, 0.62);
    backdrop-filter: blur(8px) saturate(1.4);
    -webkit-backdrop-filter: blur(8px) saturate(1.4);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.35), inset 0 0 0 1px rgba(255, 255, 255, 0.08);
    pointer-events: auto;
    animation: issue-in 0.34s var(--ease-spring);
  }
  .issue {
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
  }
  .issue.blur {
    background: #d97706;
  }
  .issue.eyes {
    background: #e5484d;
  }
  .issue.over {
    background: #c2410c;
  }
  .issue.under {
    background: #3b5bdb;
  }
  @keyframes issue-in {
    from {
      opacity: 0;
      transform: translateY(-3px) scale(0.8);
    }
  }
  .tile.poor img:global(.ld) {
    filter: saturate(0.82);
  }
  /* A series folded to its best frame: a pill with how many it holds (a click unfolds
     it), and the edges of the frames behind peeking out above the photo. */
  .tile.stacked {
    background: transparent;
  }
  .tile.stacked img {
    margin-top: 7px;
    height: calc(100% - 7px);
    border-radius: 0.375rem;
  }
  .tile.stacked .sk {
    top: 7px;
    border-radius: 0.375rem;
  }
  .sheets {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 7px;
    pointer-events: none;
  }
  .sheets::before,
  .sheets::after {
    content: '';
    position: absolute;
    height: 5px;
    border-radius: 0.3125rem 0.3125rem 0 0;
    background: color-mix(in srgb, var(--text) 22%, var(--surface-2));
  }
  .sheets::before {
    left: 12%;
    right: 12%;
    top: 0;
    opacity: 0.55;
  }
  .sheets::after {
    left: 6%;
    right: 6%;
    top: 2.5px;
    opacity: 0.85;
  }
  .tile.stacked .pills,
  .tile.stacked .corner,
  .tile.stacked .check {
    margin-top: 7px;
  }
  .tile.stacked::after,
  .tile.stacked .lc {
    top: 7px;
  }
  /* Top left: the series (its size, or that this frame is its best). */
  .pills {
    position: absolute;
    top: 0.375rem;
    left: 0.375rem;
    display: flex;
    align-items: center;
    gap: 0.25rem;
    pointer-events: none;
    transition: left 0.2s var(--ease-out);
    z-index: 2;
  }
  .pills.shifted {
    left: 2.125rem;
  }
  .stack-pill {
    pointer-events: auto;
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    height: 1.375rem;
    padding: 0 0.5rem 0 0.4375rem;
    border: 0;
    border-radius: 99px;
    background: rgba(12, 13, 16, 0.66);
    backdrop-filter: blur(8px) saturate(1.4);
    -webkit-backdrop-filter: blur(8px) saturate(1.4);
    color: #fff;
    font-size: 0.6875rem;
    font-weight: 700;
    letter-spacing: 0.01em;
    cursor: pointer;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.35), inset 0 0 0 1px rgba(255, 255, 255, 0.1);
    transition:
      background 0.15s,
      transform 0.18s var(--ease-spring);
  }
  .stack-pill:hover {
    background: var(--accent-strong);
    transform: scale(1.06);
  }
  .stack-pill.open {
    background: var(--accent-strong);
  }
  .best-pill {
    height: 1.375rem;
    display: inline-flex;
    align-items: center;
    padding: 0 0.5rem;
    border-radius: 99px;
    background: linear-gradient(135deg, #f5b301, #f08c00);
    color: #1a1200;
    font-size: 0.6875rem;
    font-weight: 800;
    letter-spacing: 0.02em;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.4);
    animation: issue-in 0.34s var(--ease-spring);
  }
  /* Narrow tiles (a phone, small thumbnails): the pills keep the top row to themselves,
     and what may be wrong goes under them on the right. */
  .tile.narrow .stack-pill {
    gap: 0.1875rem;
    padding: 0 0.375rem 0 0.3125rem;
  }
  .tile.narrow .best-pill {
    padding: 0 0.375rem;
  }
  .tile.narrow.pilled .corner {
    top: 2rem;
  }
  /* The frames of an unfolded series: a rail joins them along the bottom. */
  .rail {
    position: absolute;
    left: -3px;
    right: -3px;
    bottom: 0;
    height: 3px;
    background: var(--accent);
    opacity: 0.9;
    pointer-events: none;
    z-index: 1;
  }
  .rail.start {
    left: 0;
    border-bottom-left-radius: 3px;
  }
  .rail.end {
    right: 0;
    border-bottom-right-radius: 3px;
  }
  .tile.unfolded {
    background: color-mix(in srgb, var(--accent) 14%, var(--placeholder));
  }
  .flag {
    color: var(--pick);
    filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.7));
    transform-origin: 20% 90%;
    animation: flag-in 0.5s var(--ease-spring);
  }
  @keyframes flag-in {
    0% {
      transform: scale(0.3) rotate(-25deg);
      opacity: 0;
    }
    60% {
      transform: scale(1.15) rotate(8deg);
      opacity: 1;
    }
  }
  .check {
    position: absolute;
    top: 0.375rem;
    left: 0.375rem;
    width: 1.3125rem;
    height: 1.3125rem;
    border-radius: 50%;
    background: var(--accent-strong);
    color: #fff;
    display: grid;
    place-items: center;
    box-shadow:
      0 0 0 2px rgba(255, 255, 255, 0.9),
      0 2px 6px rgba(0, 0, 0, 0.45);
    animation: check-in 0.32s var(--ease-spring);
  }
  @keyframes check-in {
    from {
      transform: scale(0.2);
    }
  }
  /* The bottom strip: a rating (or reject mark) always shows; the file name, type and
     the shade behind them only on hover and on the keyboard cursor. */
  .meta {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    align-items: flex-end;
    gap: 0.375rem;
    padding: 1.25rem 0.375rem 0.25rem;
    color: #fff;
    font-size: 0.6875rem;
    pointer-events: none;
    isolation: isolate;
  }
  .meta::before {
    content: '';
    position: absolute;
    inset: 0;
    z-index: -1;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.55));
    opacity: 0;
    transition: opacity 0.15s;
  }
  .meta .name,
  .meta .ext {
    opacity: 0;
    transition: opacity 0.15s;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  }
  .tile:hover .meta::before,
  .tile.cursor .meta::before,
  .tile:hover .meta .name,
  .tile.cursor .meta .name {
    opacity: 1;
  }
  .tile:hover .meta .ext,
  .tile.cursor .meta .ext {
    opacity: 0.85;
  }
  .stars {
    color: var(--star);
    letter-spacing: 0.05em;
    font-size: 0.75rem;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
    transform-origin: left bottom;
  }
  .rej-r {
    color: var(--reject);
    font-weight: 700;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
  }
  .ext {
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .name {
    max-width: 60%;
  }
  .band {
    position: absolute;
    border: 1px solid var(--accent-strong);
    background: rgba(0, 112, 242, 0.15);
    border-radius: 2px;
    pointer-events: none;
    z-index: 2;
  }
  :global(body.no-select) {
    user-select: none;
    -webkit-user-select: none;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    color: var(--text-3);
    text-align: center;
    gap: 0.5rem;
  }
</style>
