<script lang="ts">
  // Reviewing one series: its frames side by side, as large as the space allows (a
  // survey). ←/→ (or a click) pick a frame, ↑/↓ go to the series before and after,
  // Enter opens the frame in the single view (which stays within the series), Esc goes
  // back to the library on the series' stack. Keep best rejects every frame but the
  // best and moves on; frames can be rejected, rated and picked one by one as anywhere.
  import { onMount } from 'svelte';
  import { ISSUE_EYES, urls } from '../lib/api';
  import { cull, focusTone, ISSUES } from '../lib/cull.svelte';
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';

  let box = $state<HTMLElement | null>(null);
  let bw = $state(0);
  let bh = $state(0);

  const L = $derived(store.list);
  const r = $derived.by(() => {
    void store.rev;
    void store.cursor;
    return cull.range();
  });
  const frames = $derived.by(() => {
    if (!r) return [] as number[];
    return Array.from({ length: r[1] - r[0] }, (_, k) => r![0] + k);
  });
  const pos = $derived.by(() => {
    void r;
    return cull.position();
  });
  const bestIdx = $derived(frames.find((i) => L.sb?.[i] === 1) ?? frames[0]);
  const span = $derived.by(() => {
    const ts = frames.map((i) => L.t[i]).filter((t): t is number => t != null);
    if (ts.length < 2) return '';
    const s = Math.max(...ts) - Math.min(...ts);
    return s < 1 ? 'within a second' : s < 90 ? `over ${s} s` : `over ${Math.round(s / 60)} min`;
  });
  const selectedHere = $derived(frames.filter((i) => store.selected.has(L.id[i])).map((i) => L.id[i]));

  // Why the best frame is the best (from its measurements).
  let why = $state('');
  $effect(() => {
    const id = bestIdx != null ? L.id[bestIdx] : undefined;
    void cull.rev;
    why = '';
    if (id == null) return;
    let live = true;
    cull.get(id).then((c) => {
      if (live) why = c?.series?.why ?? '';
    });
    return () => {
      live = false;
    };
  });

  // ------------------------------------------------------------ layout
  const GAP = 10;
  const PAD = 16;
  /** Columns and tile size that show every frame as large as possible; below a minimum
   * size the frames wrap and the view scrolls. */
  const grid = $derived.by(() => {
    const n = frames.length;
    if (!n || !bw || !bh) return { cols: 1, w: 0, h: 0 };
    const ratios = frames.map((i) => {
      const o = L.o[i];
      const w = (o >= 5 ? L.h[i] : L.w[i]) || 3;
      const h = (o >= 5 ? L.w[i] : L.h[i]) || 2;
      return w / h;
    });
    const a = ratios.sort((x, y) => x - y)[Math.floor(ratios.length / 2)];
    const W = bw - PAD * 2;
    const H = bh - PAD * 2;
    let best = { cols: 1, w: 0, h: 0 };
    for (let c = 1; c <= n; c++) {
      const rows = Math.ceil(n / c);
      const w = Math.min((W - GAP * (c - 1)) / c, ((H - GAP * (rows - 1)) / rows) * a);
      if (w > best.w) best = { cols: c, w, h: w / a };
    }
    const min = store.mobile ? (W - GAP) / 2 : 220;
    if (best.w < min) {
      const c = Math.max(1, Math.floor((W + GAP) / (min + GAP)));
      const w = (W - GAP * (c - 1)) / c;
      best = { cols: c, w, h: w / a };
    }
    return best;
  });
  /** Tiles this wide (device pixels) get a preview instead of the thumbnail. */
  const big = $derived(grid.w * (window.devicePixelRatio || 1) > 520);

  // The picked frame stays in view.
  $effect(() => {
    const c = store.cursor;
    void grid;
    queueMicrotask(() => box?.querySelector<HTMLElement>(`[data-i="${c}"]`)?.scrollIntoView({ block: 'nearest' }));
  });

  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (!box) return;
      bw = box.clientWidth;
      bh = box.clientHeight;
    });
    if (box) ro.observe(box);
    return () => ro.disconnect();
  });

  function pick(i: number, e: MouseEvent) {
    if (e.metaKey || e.ctrlKey) {
      store.select(i, 'toggle');
      return;
    }
    if (e.shiftKey) {
      store.extendTo(i);
      return;
    }
    if (i === store.cursor && store.mobile) {
      store.openLoupe(i);
      return;
    }
    store.moveCursor(i);
  }

  const pct = (f: number) => `${Math.round(f)}%`;
  const loaded = (e: Event) => (e.currentTarget as HTMLElement).classList.add('ld');
</script>

<div class="series" role="region" aria-label="Series review">
  <header class="bar">
    <button class="btn ghost sm back" onclick={() => cull.leave()} title={`Back to the library${store.keyHint('escape')}`}>
      <Icon name="chevron-left" size={16} /><span class="hide-m">Library</span>
    </button>
    <div class="title">
      <h2>Series {#if pos}<span class="tnum">{pos.n}</span> <span class="dim">of {pos.of}</span>{/if}</h2>
      <span class="meta dim tnum">{frames.length} frames{span ? ` ${span}` : ''}</span>
    </div>
    <div class="nav">
      <button class="btn ghost icon sm" onclick={() => cull.step(-1)} disabled={!pos || pos.n <= 1} title="Previous series (↑)" aria-label="Previous series"><Icon name="chevron-up" size={16} /></button>
      <button class="btn ghost icon sm" onclick={() => cull.step(1)} disabled={!pos || pos.n >= pos.of} title="Next series (↓)" aria-label="Next series"><Icon name="chevron-down" size={16} /></button>
    </div>
    <span class="grow"></span>
    {#if selectedHere.length}
      <button class="btn sm" onclick={() => cull.keep(selectedHere)} title="Keep the selected frames, reject the others, next series">
        <Icon name="check" size={14} />Keep {selectedHere.length} selected
      </button>
    {/if}
    <button class="btn primary sm" onclick={() => cull.keepBestHere()} disabled={frames.length < 2} title={`Keep the best frame, reject the others, next series${store.keyHint('keepBest')}`}>
      <Icon name="sparkles" size={14} />Keep best
    </button>
  </header>

  <div class="frames" bind:this={box}>
    <div class="wrap" style:grid-template-columns={`repeat(${grid.cols}, ${grid.w}px)`} style:padding={`${PAD}px`} style:gap={`${GAP}px`}>
      {#each frames as i, k (L.id[i])}
        {@const id = L.id[i]}
        {@const _r = store.rev}
        {@const cq = L.cq?.[i] ?? -1}
        {@const cf = L.cf?.[i] ?? -1}
        {@const isBest = i === bestIdx}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="tile"
          class:cur={i === store.cursor}
          class:sel={store.selected.has(id)}
          class:best={isBest}
          class:leaving={store.leaving.has(id)}
          style:width={`${grid.w}px`}
          style:height={`${grid.h}px`}
          data-i={i}
          role="button"
          tabindex="-1"
          aria-label={`${L.n[i]}${isBest ? ', best frame' : ''}`}
          aria-pressed={i === store.cursor}
          onclick={(e) => pick(i, e)}
          ondblclick={() => store.openLoupe(i)}
        >
          <img class="th" src={urls.thumb(id, L.v[i], store.thumbRev[id])} alt="" draggable="false" onload={loaded} />
          {#if big}<img class="pv" src={urls.preview(id, L.v[i], 1024)} alt="" draggable="false" loading="lazy" onload={loaded} />{/if}
          <div class="top">
            <span class="no tnum">{k + 1}</span>
            {#if isBest}<span class="bestchip"><Icon name="sparkles" size={11} />Best</span>{/if}
            <span class="grow"></span>
            {#if store.selected.has(id)}<span class="check"><Icon name="check" size={12} stroke={3} /></span>{/if}
          </div>
          <div class="bottom">
            {#if cf >= 0}
              <span class="sharp {focusTone(cf / 100)}" title="Sharpness of the subject">
                <span class="meter"><span style:width={pct(cf)}></span></span><span class="tnum">{pct(cf)}</span>
              </span>
            {:else if cq < 0}
              <span class="dim small">Measuring…</span>
            {/if}
            {#if cq > 0}
              {#each ISSUES as is (is.key)}
                {#if cq & is.bit}<span class="issue {is.key}" title={is.label}><Icon name={is.icon} size={13} stroke={2.2} /></span>{/if}
              {/each}
            {:else if cq === 0 && L.cq && !(cq & ISSUE_EYES)}
              <span class="ok" title="No problems found"><Icon name="check" size={13} stroke={2.4} /></span>
            {/if}
            <span class="grow"></span>
            {#if L.r[i] > 0 && _r >= 0}<span class="stars">{'★'.repeat(L.r[i])}</span>{/if}
            {#if L.p[i] === 1}<span class="flag"><Icon name="flag" size={13} fill /></span>{/if}
            {#if L.lc[i]}<span class="lc label-{L.lc[i]}"></span>{/if}
          </div>
          <div class="acts">
            <button class="btn icon xs" onclick={(e) => { e.stopPropagation(); cull.keep([id]); }} title="Keep only this frame, reject the others, next series" aria-label="Keep only this frame"><Icon name="check" size={14} stroke={2.4} /></button>
            <button class="btn icon xs danger" onclick={(e) => { e.stopPropagation(); store.reject([id]); }} title={`Reject this frame${store.keyHint('reject')}`} aria-label="Reject this frame"><Icon name="x" size={14} stroke={2.4} /></button>
          </div>
        </div>
      {/each}
    </div>
  </div>

  <footer class="foot">
    {#if why && bestIdx != null}
      <span class="why"><Icon name="sparkles" size={12} /> Best is frame {frames.indexOf(bestIdx) + 1}: {why}</span>
    {/if}
    <span class="grow"></span>
    {#if !store.mobile}
      <span class="keys dim">
        <span><kbd>←</kbd><kbd>→</kbd> frames</span>
        <span><kbd>↑</kbd><kbd>↓</kbd> series</span>
        <span><kbd>Enter</kbd> view</span>
        {#if store.keyHint('keepBest')}<span><kbd>{store.keyHint('keepBest').slice(2, -1)}</kbd> keep best</span>{/if}
        <span><kbd>Esc</kbd> library</span>
      </span>
    {/if}
  </footer>
</div>

<style>
  .series {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    height: var(--toolbar-h);
    padding: 0 0.75rem 0 0.375rem;
    border-bottom: 1px solid var(--border);
    flex: none;
  }
  .back {
    gap: 0.25rem;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 0.625rem;
    min-width: 0;
  }
  .title h2 {
    margin: 0;
    font-size: var(--fs-lg);
    font-weight: 650;
    white-space: nowrap;
  }
  .meta {
    font-size: var(--fs-sm);
    white-space: nowrap;
  }
  .nav {
    display: flex;
    gap: 0.125rem;
    margin-left: 0.25rem;
  }
  .grow {
    flex: 1;
  }
  .frames {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .wrap {
    display: grid;
    justify-content: center;
    align-content: center;
    min-height: 100%;
    box-sizing: border-box;
  }
  .tile {
    position: relative;
    border-radius: var(--r);
    overflow: hidden;
    background: var(--placeholder);
    cursor: pointer;
    outline: 2px solid transparent;
    outline-offset: 2px;
    transition:
      outline-color var(--dur-fast),
      opacity var(--dur),
      transform var(--dur) var(--ease-out);
  }
  .tile img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    opacity: 0;
    transition: opacity var(--dur);
  }
  .tile img:global(.ld) {
    opacity: 1;
  }
  .tile:not(.cur) img {
    filter: brightness(0.82);
  }
  .tile:hover:not(.cur) img {
    filter: none;
  }
  .tile.cur {
    outline-color: var(--accent);
  }
  .tile.sel {
    outline-color: var(--text-2);
  }
  .tile.cur.sel {
    outline-color: var(--accent);
  }
  .tile.leaving {
    opacity: 0;
    transform: scale(0.94);
  }
  .top,
  .bottom {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 0.375rem;
    padding: 0.4375rem 0.5rem;
    pointer-events: none;
    font-size: var(--fs-sm);
    color: #fff;
  }
  .top {
    top: 0;
    background: linear-gradient(rgba(0, 0, 0, 0.45), transparent);
  }
  .bottom {
    bottom: 0;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.6));
    padding-top: 1.25rem;
  }
  .no {
    min-width: 1.375rem;
    height: 1.375rem;
    padding: 0 0.3125rem;
    display: inline-grid;
    place-items: center;
    border-radius: 0.6875rem;
    background: rgba(0, 0, 0, 0.5);
    font-size: var(--fs-xs);
    font-weight: 650;
  }
  .tile.cur .no {
    background: var(--accent-strong);
  }
  .bestchip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    height: 1.375rem;
    padding: 0 0.5rem;
    border-radius: 0.6875rem;
    background: var(--positive);
    color: #06210f;
    font-size: var(--fs-xs);
    font-weight: 700;
  }
  .check {
    width: 1.375rem;
    height: 1.375rem;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--accent-strong);
  }
  .sharp {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    font-weight: 600;
  }
  .meter {
    width: 2.75rem;
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.25);
    overflow: hidden;
  }
  .meter > span {
    display: block;
    height: 100%;
    background: currentColor;
  }
  .sharp.good {
    color: #6ee7a0;
  }
  .sharp.fair {
    color: #f7c35f;
  }
  .sharp.poor {
    color: #ff8a8a;
  }
  .issue {
    display: inline-grid;
    place-items: center;
    width: 1.375rem;
    height: 1.375rem;
    border-radius: 50%;
    background: rgba(255, 99, 99, 0.9);
  }
  .issue.over {
    background: rgba(247, 195, 95, 0.95);
    color: #2a1d00;
  }
  .issue.under {
    background: rgba(90, 110, 255, 0.9);
  }
  .ok {
    display: inline-grid;
    place-items: center;
    color: #6ee7a0;
  }
  .stars {
    color: var(--star);
    letter-spacing: -0.05em;
  }
  .flag {
    color: var(--pick);
    display: inline-flex;
  }
  .lc {
    width: 0.625rem;
    height: 0.625rem;
    border-radius: 50%;
  }
  .small {
    font-size: var(--fs-xs);
  }
  .dim.small {
    color: rgba(255, 255, 255, 0.7);
  }
  .acts {
    position: absolute;
    top: 0.4375rem;
    right: 0.5rem;
    display: flex;
    gap: 0.25rem;
    opacity: 0;
    transition: opacity var(--dur-fast);
  }
  .tile:hover .acts,
  .tile.cur .acts {
    opacity: 1;
  }
  .tile.sel .acts {
    right: 2.25rem;
  }
  .xs {
    width: 1.75rem;
    height: 1.75rem;
    min-width: 1.75rem;
    padding: 0;
    border-radius: 50%;
    background: rgba(0, 0, 0, 0.55);
    color: #fff;
    border: 0;
    backdrop-filter: blur(4px);
  }
  .xs:hover {
    background: rgba(0, 0, 0, 0.8);
  }
  .xs.danger:hover {
    background: var(--reject);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 1rem;
    min-height: 2.25rem;
    padding: 0.375rem 0.875rem;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
    flex: none;
  }
  .why {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    color: var(--text-2);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .why :global(svg) {
    color: var(--positive);
    flex: none;
  }
  .keys {
    display: flex;
    gap: 1rem;
    white-space: nowrap;
  }
  .keys > span {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  kbd {
    font: inherit;
    font-size: var(--fs-xs);
    min-width: 1.25rem;
    height: 1.25rem;
    padding: 0 0.3125rem;
    display: inline-grid;
    place-items: center;
    border-radius: 0.25rem;
    border: 1px solid var(--border-strong);
    background: var(--surface-2);
    color: var(--text-2);
  }
  @media (max-width: 760px) {
    .hide-m {
      display: none;
    }
    .meta {
      display: none;
    }
    .acts {
      opacity: 1;
    }
  }
</style>
