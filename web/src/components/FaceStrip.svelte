<script lang="ts">
  // The faces of a photo side by side: each person of the photo followed through every
  // frame of its series, so the frame where everyone's eyes are open and sharp shows at
  // a glance. A click goes to that frame; hovering a face shows it larger.
  import { cull, focusTone, focusWord } from '../lib/cull.svelte';
  import { store } from '../lib/store.svelte';
  import { urls, type CullFace, type CullPhoto } from '../lib/api';
  import { reduced } from '../lib/motion';
  import Icon from './Icon.svelte';

  let { photo }: { photo: number } = $props();

  let data = $state<CullPhoto | null>(null);
  let shownFor = $state<number | null>(null);

  $effect(() => {
    const id = photo;
    void cull.rev;
    let live = true;
    cull.get(id).then((c) => {
      if (!live) return;
      data = c;
      shownFor = id;
    });
    return () => {
      live = false;
    };
  });

  const series = $derived(data?.series ?? null);
  /** Rows: one per face of this photo; columns: the frames (or just this photo). */
  const rows = $derived.by(() => {
    if (!data) return [];
    if (series) return series.tracks.map((t, k) => ({ label: t.subject?.name ?? `Face ${k + 1}`, named: !!t.subject?.name, faces: t.faces }));
    return data.faces.map((f, k) => ({ label: f.subject?.name ?? `Face ${k + 1}`, named: !!f.subject?.name, faces: [f] as (CullFace | null)[] }));
  });
  const frames = $derived(series ? series.frames : data ? [{ id: data.id, name: '', v: '', w: null, h: null, o: 1, rating: 0, pick: 0, focus: data.focus, closed: data.eyes?.closed ?? null, issues: data.issues }] : []);
  const cur = $derived(series ? series.frames.findIndex((f) => f.id === photo) : 0);
  const visible = $derived(rows.length > 0 && shownFor === photo);
  const ro = $derived(!!store.status?.readonly);

  let hover = $state<{ face: CullFace; x: number; y: number } | null>(null);

  function enter(face: CullFace, e: PointerEvent) {
    if (e.pointerType !== 'mouse') return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    hover = { face, x: r.left + r.width / 2, y: r.top };
  }

  function go(k: number) {
    const f = frames[k];
    if (!f || f.id === photo) return;
    cull.showFrame(f.id, series?.key ?? 0);
  }

  const pct = (v: number | null | undefined) => (v == null ? '–' : `${Math.round(v * 100)}%`);
</script>

{#if visible}
  <div class="strip" class:single={!series}>
    <div class="head">
      <span class="title"><Icon name="scan-face" size={14} />{series ? `Faces across ${series.frames.length} frames` : rows.length === 1 ? 'Face' : `${rows.length} faces`}</span>
      {#if series}
        <span class="best">
          <span class="medal">Best</span>
          frame {series.frames.findIndex((f) => f.id === series.best) + 1}{#if series.why}<span class="why">· {series.why.toLowerCase()}</span>{/if}
        </span>
        <span class="grow"></span>
        {#if series.best !== photo}
          <button class="btn sm ghost" onclick={() => cull.showFrame(series.best, series.key)}><Icon name="arrow-up-right" />Go to best</button>
        {/if}
        {#if !ro && data}
          <button class="btn sm keep" onclick={() => data && cull.keepBest(data)} title={'Keep the best frame and move the other ' + (series.frames.length - 1) + ' to the reject folder (undoable)' + store.keyHint('keepBest')}>
            <Icon name="check" />Keep best, reject {series.frames.length - 1}
          </button>
        {/if}
      {:else}
        <span class="grow"></span>
      {/if}
    </div>
    <div class="grid" style:--cols={frames.length}>
      {#if series}
        <span class="corner"></span>
        {#each frames as f, k (f.id)}
          {@const bad = f.issues.filter((x) => x === 'blur' || x === 'eyes')}
          <button class="fno" class:cur={k === cur} class:isbest={f.id === series.best} onclick={() => go(k)} title={f.name}>
            {k + 1}{#if bad.length}<span class="dots">{#each bad as is (is)}<span class="dot {is}"></span>{/each}</span>{/if}
          </button>
        {/each}
      {/if}
      {#each rows as row, r (r)}
        <span class="who" class:named={row.named} title={row.label}>{row.label}</span>
        {#each row.faces as face, k (k)}
          {#if face}
            <button
              class="cell {focusTone(face.focus)}"
              class:cur={k === cur}
              class:shut={face.shut}
              class:isbest={series != null && frames[k]?.id === series.best}
              style:--i={r * frames.length + k}
              onclick={() => go(k)}
              onpointerenter={(e) => enter(face, e)}
              onpointerleave={() => (hover = null)}
              aria-label={`${row.label}, frame ${k + 1}: ${face.shut ? 'eyes closed, ' : ''}${focusWord(face.focus).toLowerCase()}`}
            >
              <img src={urls.crop(face, true)} alt="" loading="lazy" draggable="false" />
              {#if face.shut}<span class="eyeflag" title="Eyes closed"><Icon name="eye-closed" size={12} stroke={2.4} /></span>{/if}
              <span class="meter" title={`${focusWord(face.focus)} (${pct(face.focus)})`}><span style:width={`${Math.round((face.focus ?? 0) * 100)}%`}></span></span>
            </button>
          {:else}
            <span class="cell none" title="Not found in this frame"></span>
          {/if}
        {/each}
      {/each}
    </div>
  </div>
  {#if hover}
    <div class="peek" style:left={`${hover.x}px`} style:top={`${hover.y}px`} class:still={reduced()}>
      <img src={urls.crop(hover.face, true)} alt="" />
      <div class="peek-meta">
        <span class="tag {focusTone(hover.face.focus)}">{focusWord(hover.face.focus)} {pct(hover.face.focus)}</span>
        {#if hover.face.closed != null}<span class="tag" class:bad={hover.face.shut}>{hover.face.shut ? 'Eyes closed' : 'Eyes open'}</span>{/if}
      </div>
    </div>
  {/if}
{/if}

<style>
  .strip {
    flex: none;
    max-height: 15.5rem;
    overflow: auto;
    padding: 0.5rem 0.875rem 0.625rem;
    background: var(--loupe-panel, rgba(14, 15, 19, 0.96));
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    color: #e8e9ee;
    animation: strip-in 0.32s var(--ease-out);
  }
  @keyframes strip-in {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    margin-bottom: 0.4375rem;
    font-size: var(--fs-sm);
  }
  .title {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    font-weight: 650;
    color: #fff;
  }
  .best {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    color: #b7bac3;
  }
  .why {
    color: #8b8f99;
  }
  .medal {
    display: inline-flex;
    align-items: center;
    height: 1.125rem;
    padding: 0 0.4375rem;
    border-radius: 99px;
    background: linear-gradient(135deg, #f5b301, #f08c00);
    color: #1a1200;
    font-size: 0.625rem;
    font-weight: 800;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .grow {
    flex: 1;
  }
  .head .btn.ghost {
    color: #c9ccd4;
  }
  .head .btn.ghost:hover {
    background: rgba(255, 255, 255, 0.08);
    color: #fff;
  }
  .keep {
    background: rgba(69, 208, 140, 0.14);
    border-color: rgba(69, 208, 140, 0.4);
    color: #6be3a6;
  }
  .keep:hover {
    background: rgba(69, 208, 140, 0.24);
  }
  .grid {
    display: grid;
    grid-template-columns: 5.5rem repeat(var(--cols), 4.5rem);
    gap: 0.375rem 0.4375rem;
    align-items: center;
  }
  .single .grid {
    grid-template-columns: 5.5rem 4.5rem;
  }
  .corner {
    height: 1rem;
  }
  .fno {
    position: relative;
    height: 1.25rem;
    border: 0;
    border-radius: 0.3125rem;
    background: transparent;
    color: #8b8f99;
    font: inherit;
    font-size: 0.6875rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 0.25rem;
  }
  .fno:hover {
    color: #fff;
    background: rgba(255, 255, 255, 0.06);
  }
  .fno.cur {
    color: #fff;
    background: color-mix(in srgb, var(--accent-strong) 55%, transparent);
  }
  .fno.isbest::before {
    content: '★';
    color: #f5b301;
    margin-right: 0.125rem;
  }
  .dots {
    display: inline-flex;
    gap: 2px;
  }
  .dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
  }
  .dot.blur {
    background: #f59e0b;
  }
  .dot.eyes {
    background: #ff5c61;
  }
  .dot.over {
    background: #fb7a3c;
  }
  .dot.under {
    background: #6b8cff;
  }
  .who {
    font-size: var(--fs-sm);
    color: #8b8f99;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .who.named {
    color: #e8e9ee;
    font-weight: 600;
  }
  .cell {
    position: relative;
    width: 4.5rem;
    height: 4.5rem;
    padding: 0;
    border: 0;
    border-radius: 0.5rem;
    overflow: hidden;
    background: rgba(255, 255, 255, 0.05);
    cursor: pointer;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.06);
    animation: cell-in 0.36s var(--ease-out) backwards;
    animation-delay: calc(min(var(--i, 0), 24) * 18ms);
    transition:
      transform 0.18s var(--ease-spring),
      box-shadow 0.18s;
  }
  @keyframes cell-in {
    from {
      opacity: 0;
      transform: scale(0.92);
    }
  }
  .cell:hover {
    transform: translateY(-2px);
  }
  .cell img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .cell::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08);
    transition: box-shadow 0.18s;
  }
  .cell.cur::after {
    box-shadow:
      inset 0 0 0 2px var(--accent),
      inset 0 0 0 3px rgba(0, 0, 0, 0.5);
  }
  .cell.isbest:not(.cur)::after {
    box-shadow: inset 0 0 0 2px rgba(245, 179, 1, 0.75);
  }
  .cell.shut img {
    filter: saturate(0.75);
  }
  .cell.shut::after {
    box-shadow: inset 0 0 0 2px rgba(255, 92, 97, 0.85);
  }
  .cell.cur.shut::after {
    box-shadow:
      inset 0 0 0 2px var(--accent),
      inset 0 0 0 4px rgba(255, 92, 97, 0.85);
  }
  .cell.none {
    cursor: default;
    background: repeating-linear-gradient(135deg, rgba(255, 255, 255, 0.03) 0 6px, transparent 6px 12px);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.05);
  }
  .eyeflag {
    position: absolute;
    top: 0.25rem;
    right: 0.25rem;
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: #e5484d;
    color: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
    animation: pop 0.32s var(--ease-spring);
  }
  @keyframes pop {
    from {
      transform: scale(0.3);
    }
  }
  .meter {
    position: absolute;
    left: 0.3125rem;
    right: 0.3125rem;
    bottom: 0.3125rem;
    height: 3px;
    border-radius: 3px;
    background: rgba(0, 0, 0, 0.45);
    overflow: hidden;
  }
  .meter span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: #8b8f99;
    transition: width 0.4s var(--ease-out);
  }
  .good .meter span {
    background: #45d08c;
  }
  .fair .meter span {
    background: #f5b301;
  }
  .poor .meter span {
    background: #ff5c61;
  }
  .peek {
    position: fixed;
    z-index: 60;
    transform: translate(-50%, calc(-100% - 10px));
    width: 13rem;
    border-radius: 0.75rem;
    overflow: hidden;
    background: #111217;
    box-shadow: 0 24px 50px -12px rgba(0, 0, 0, 0.8), 0 0 0 1px rgba(255, 255, 255, 0.1);
    pointer-events: none;
    animation: peek-in 0.2s var(--ease-out);
  }
  .peek.still {
    animation: none;
  }
  @keyframes peek-in {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-100% - 4px)) scale(0.94);
    }
  }
  .peek img {
    width: 100%;
    aspect-ratio: 1;
    object-fit: cover;
    display: block;
  }
  .peek-meta {
    display: flex;
    gap: 0.375rem;
    padding: 0.4375rem 0.5rem;
    font-size: 0.6875rem;
    font-weight: 650;
  }
  .tag {
    padding: 0.125rem 0.4375rem;
    border-radius: 99px;
    background: rgba(255, 255, 255, 0.08);
    color: #d6d8de;
  }
  .tag.good {
    color: #6be3a6;
  }
  .tag.fair {
    color: #f7c35f;
  }
  .tag.poor,
  .tag.bad {
    color: #ff8589;
  }
</style>
