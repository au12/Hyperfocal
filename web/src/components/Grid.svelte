<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { urls } from '../lib/api';
  import { aspectOf, justify, rowAt } from '../lib/layout';
  import { gridCtl } from '../lib/ctl';
  import Icon from './Icon.svelte';

  let scroller: HTMLDivElement | undefined = $state();
  let width = $state(0);
  let viewH = $state(0);
  let scrollTop = $state(0);
  let selecting = $state(false);
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

  const visible = $derived.by(() => {
    const rows = layout.rows;
    if (!rows.length) return [];
    const over = viewH * 1.2;
    const a = rowAt(rows, Math.max(0, scrollTop - pad - over));
    const out = [];
    for (let r = a; r < rows.length; r++) {
      if (rows[r].top > scrollTop - pad + viewH + over) break;
      out.push(rows[r]);
    }
    return out;
  });

  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (!scroller) return;
      width = scroller.clientWidth;
      viewH = scroller.clientHeight;
    });
    if (scroller) ro.observe(scroller);
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
    return () => ro.disconnect();
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

  let pressTimer: ReturnType<typeof setTimeout> | undefined;
  let longPressed = false;

  function onDown(i: number, e: PointerEvent) {
    if (e.pointerType !== 'mouse') {
      longPressed = false;
      pressTimer = setTimeout(() => {
        longPressed = true;
        selecting = true;
        store.select(i, 'toggle');
        navigator.vibrate?.(15);
      }, 450);
    }
  }
  function onUp() {
    clearTimeout(pressTimer);
  }

  function onClick(i: number, e: MouseEvent) {
    if (longPressed) {
      longPressed = false;
      return;
    }
    if (store.mobile) {
      if (selecting || store.selected.size > 0) {
        store.select(i, 'toggle');
        if (store.selected.size === 0) selecting = false;
      } else store.openLoupe(i);
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
    updateBand();
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
          ondblclick={() => store.openLoupe(i)}
          onpointerdown={(e) => onDown(i, e)}
          onpointerup={onUp}
          onpointercancel={onUp}
          onpointerleave={onUp}
          oncontextmenu={(e) => store.mobile && e.preventDefault()}
          role="gridcell"
          tabindex="-1"
          aria-selected={store.selected.has(id) || (store.selected.size === 0 && i === store.cursor)}
          title={store.list.n[i]}
        >
          <img src={urls.thumb(id, store.list.v[i], store.thumbRev[id])} alt="" loading="lazy" decoding="async" draggable="false" />
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
  .meta {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    align-items: flex-end;
    gap: 0.375rem;
    padding: 1.25rem 0.375rem 0.25rem;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.55));
    color: #fff;
    font-size: 0.6875rem;
    opacity: 0;
    transition: opacity 0.15s;
    pointer-events: none;
  }
  .tile:hover .meta,
  .tile.cursor .meta,
  .meta:has(.stars),
  .meta:has(.rej-r) {
    opacity: 1;
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
  }
  .ext {
    font-weight: 700;
    letter-spacing: 0.02em;
    opacity: 0.85;
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
