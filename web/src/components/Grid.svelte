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
    gridCtl.vertical = (d: number) => {
      const i = store.cursor;
      const r = layout.rowOf[i] ?? 0;
      const row = layout.rows[r];
      const next = layout.rows[r + d];
      if (!row || !next) return;
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
      store.cursor = best;
    };
    gridCtl.page = (d: number) => {
      if (!scroller) return;
      scroller.scrollBy({ top: d * (viewH - target) });
      const y = scroller.scrollTop + d * (viewH - target);
      const r = rowAt(layout.rows, Math.max(0, y));
      const row = layout.rows[r];
      if (row) store.cursor = row.start;
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
    if (e.shiftKey) store.select(i, 'range');
    else if (e.ctrlKey || e.metaKey) store.select(i, 'toggle');
    else {
      store.cursor = i;
      store.anchor = i;
      if (store.selected.size) store.clearSelection();
    }
  }

  function stars(n: number) {
    return n > 0 ? '★'.repeat(n) : '';
  }
</script>

<div class="scroller" bind:this={scroller} onscroll={() => (scrollTop = scroller?.scrollTop ?? 0)} tabindex="-1">
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
          aria-selected={store.selected.has(id)}
          title={store.list.n[i]}
        >
          <img src={urls.thumb(id, store.list.v[i])} alt="" loading="lazy" decoding="async" draggable="false" />
          {#if store.list.lc[i] && _r >= 0}<span class="lc label-{store.list.lc[i]}"></span>{/if}
          {#if store.list.p[i] === 1}<span class="flag"><Icon name="flag" size={14} fill /></span>{/if}
          {#if store.selected.has(id)}<span class="check"><Icon name="check" size={14} /></span>{/if}
          <div class="meta">
            {#if store.list.r[i] > 0}<span class="stars">{stars(store.list.r[i])}</span>{:else if store.list.r[i] < 0}<span class="rej-r">✕</span>{/if}
            <span class="grow"></span>
            {#if store.settings.showNames}<span class="name ellipsis">{store.list.n[i]}</span>{/if}
            <span class="ext">{store.list.x[i].toUpperCase().replace('+', '+JPG')}</span>
          </div>
        </div>
      {/each}
    {/each}
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
