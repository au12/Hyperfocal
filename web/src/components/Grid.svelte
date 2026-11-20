<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { urls } from '../lib/api';
  import { aspectOf, justify, rowAt } from '../lib/layout';
  import { gridCtl } from '../lib/ctl';
  import { thumb, thumbs } from '../lib/thumbs';
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
  <div class="spacer" style:height={`${layout.height + pad * 2}px`}>
    {#each visible as row (row.start)}
      {#each { length: row.end - row.start } as _, k (store.list.id[row.start + k])}
        {@const i = row.start + k}
        {@const id = store.list.id[i]}
        {@const _r = store.rev}
        <!-- Keyboard culling is handled by the global key map (arrows, ratings, P/X…). -->
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="tile"
          class:cursor={i === store.cursor}
          class:sel={store.selected.has(id)}
          class:rejected={store.folders.get(store.list.f[i])?.isReject}
          style:left={`${pad + row.x[k]}px`}
          style:top={`${pad + row.top}px`}
          style:width={`${row.w[k]}px`}
          style:height={`${row.height}px`}
          onclick={(e) => onClick(i, e)}
          ondblclick={() => !store.mobile && store.openLoupe(i)}
          onpointerdown={(e) => onDown(i, e)}
          oncontextmenu={(e) => store.mobile && e.preventDefault()}
          role="gridcell"
          tabindex="-1"
          aria-selected={store.selected.has(id) || (store.selected.size === 0 && i === store.cursor)}
          title={store.list.n[i]}
        >
          <img use:thumb={{ url: urls.thumb(id, store.list.v[i], store.thumbRev[id]), i }} alt="" decoding="async" draggable="false" />
          {#if store.list.lc[i] && _r >= 0}<span class="lc label-{store.list.lc[i]}"></span>{/if}
          {#if store.list.p[i] === 1}<span class="flag"><Icon name="flag" size={14} fill /></span>{/if}
          {#if store.selected.has(id)}<span class="check"><Icon name="check" size={14} /></span>{/if}
          <div class="meta">
            {#if store.list.r[i] > 0}<span class="stars">{stars(store.list.r[i])}</span>{:else if store.list.r[i] < 0}<span class="rej-r">✕</span>{/if}
            <span class="grow"></span>
            {#if store.settings.showNames}<span class="name ellipsis">{store.list.n[i]}</span>{/if}
            <span class="ext">{store.list.x[i].toUpperCase()}</span>
          </div>
        </div>
      {/each}
    {/each}
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
    overflow: hidden;
    border-radius: 0.25rem;
    background: var(--placeholder);
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
    -webkit-touch-callout: none;
    contain: layout paint;
  }
  .tile img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    transition: opacity 0.15s;
  }
  .tile::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
    transition: box-shadow 0.1s;
  }
  .tile:hover::after {
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.35);
  }
  .tile.cursor::after {
    box-shadow: inset 0 0 0 2px var(--accent), inset 0 0 0 3px rgba(0, 0, 0, 0.5);
  }
  .tile.sel::after {
    box-shadow: inset 0 0 0 3px var(--accent-strong);
    background: rgba(0, 112, 242, 0.14);
  }
  .tile.rejected img {
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
  .flag {
    position: absolute;
    top: 0.375rem;
    right: 0.375rem;
    color: var(--pick);
    filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.7));
  }
  .check {
    position: absolute;
    top: 0.375rem;
    left: 0.375rem;
    width: 1.375rem;
    height: 1.375rem;
    border-radius: 50%;
    background: var(--accent-strong);
    color: #fff;
    display: grid;
    place-items: center;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
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
