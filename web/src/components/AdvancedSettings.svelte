<script lang="ts">
  // Settings → Advanced: the numbers behind series, the best frame, possible rejects,
  // Find similar, search and the Duplicates report. Changes are a draft until applied
  // (they change the whole library's series and badges); the series part shows what
  // the draft would make of the folder open in the grid while a value moves.
  import { onDestroy, untrack } from 'svelte';
  import { api, type SeriesStats, type Tuning } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { tuning } from '../lib/tuning.svelte';
  import Icon from './Icon.svelte';

  type Key = keyof Tuning;
  interface Field {
    k: Key;
    label: string;
    hint: string;
    kind: 'range' | 'switch' | 'num';
    min?: number;
    max?: number;
    step?: number;
    /** The value in words or units. */
    fmt?: (v: number) => string;
    /** Words at the ends of a slider. */
    ends?: [string, string];
  }
  interface Group {
    id: string;
    title: string;
    blurb: string;
    fields: Field[];
  }

  const pct = (v: number) => `${Math.round(v * 100)}%`;
  const pct1 = (v: number) => `${(v * 100).toFixed(v < 0.1 ? 1 : 0)}%`;
  const secs = (v: number) => (v === 0 ? 'Same second' : v < 60 ? `${+v.toFixed(1)} s` : v % 60 === 0 ? `${v / 60} min` : `${Math.floor(v / 60)} min ${v % 60} s`);
  const cos = (v: number) => v.toFixed(v >= 0.9 ? 3 : 2);

  const groups: Group[] = [
    {
      id: 'series',
      title: 'Series',
      blurb: 'How frames of one moment (a burst, a run of attempts, a panorama sweep) are stacked in culling mode.',
      fields: [
        { k: 'series_burst', label: 'Burst', hint: 'Frames this close together stay in one series unless the scene clearly changes.', kind: 'range', min: 0, max: 10, step: 0.5, fmt: secs },
        { k: 'series_gap', label: 'Longest pause', hint: 'After a longer pause a new series always starts. Up to it, the frames have to look alike, more so the longer the pause.', kind: 'range', min: 0, max: 600, step: 5, fmt: secs },
        { k: 'series_strictness', label: 'Likeness after a pause', hint: 'How alike frames must look to join across a pause.', kind: 'range', min: 0, max: 1, step: 0.05, fmt: pct, ends: ['Loose', 'Strict'] },
        { k: 'series_scene', label: 'Split a burst', hint: 'Within a burst, a frame this unlike the last ones starts a new series (0: a burst is never split).', kind: 'range', min: 0, max: 1, step: 0.05, fmt: (v) => (v === 0 ? 'Never' : pct(v)), ends: ['Never', 'Often'] },
        { k: 'series_min', label: 'Fewest frames', hint: 'Shorter runs stay single photos.', kind: 'num', min: 2, max: 50, step: 1 },
        { k: 'series_cameras', label: 'Mix cameras', hint: 'Frames from two bodies on one subject can share a series.', kind: 'switch' },
      ],
    },
    {
      id: 'best',
      title: 'Best frame',
      blurb: 'Which frame tops a series and is kept by Keep best.',
      fields: [
        { k: 'best_eyes_first', label: 'Open eyes first', hint: 'A frame with everyone’s eyes open beats a sharper one with closed eyes.', kind: 'switch' },
        { k: 'best_tie', label: 'Equally sharp within', hint: 'Smaller sharpness differences are a tie; the earlier frame wins it.', kind: 'range', min: 0, max: 0.3, step: 0.01, fmt: pct },
      ],
    },
    {
      id: 'rejects',
      title: 'Possible rejects',
      blurb: 'When a photo gets a badge and shows under Culling → Possible rejects. Nothing is ever rejected by itself.',
      fields: [
        { k: 'cull_blurry', label: 'Blurry below', hint: 'Subjects (faces first) less sharp than this are marked blurry.', kind: 'range', min: 0, max: 1, step: 0.05, fmt: pct },
        { k: 'cull_sharp', label: 'Sharp from', hint: 'Subjects at least this sharp are called sharp in the single view.', kind: 'range', min: 0, max: 1, step: 0.05, fmt: pct },
        { k: 'cull_closed', label: 'Eyes closed from', hint: 'The closed-eye score from which a face counts as blinking. Lower catches more half-closed eyes.', kind: 'range', min: 0.05, max: 0.95, step: 0.05, fmt: pct, ends: ['Catch more', 'Catch fewer'] },
        { k: 'cull_clipped', label: 'Clipped when', hint: 'Share of the photo with clipped highlights.', kind: 'range', min: 0.005, max: 0.3, step: 0.005, fmt: pct1 },
        { k: 'cull_dark', label: 'Too dark when', hint: 'Share of the photo deep in the shadows.', kind: 'range', min: 0.1, max: 1, step: 0.05, fmt: pct },
      ],
    },
    {
      id: 'similar',
      title: 'Similar and search',
      blurb: 'Find similar and search by content (needs the AI engine).',
      fields: [
        { k: 'like_floor', label: 'Find similar', hint: 'How alike a photo must look to be listed.', kind: 'range', min: 0.3, max: 0.98, step: 0.01, fmt: cos, ends: ['More results', 'Closer matches'] },
        { k: 'search_floor', label: 'Search by content', hint: 'How close to the words a photo must be.', kind: 'range', min: 0, max: 0.2, step: 0.005, fmt: (v) => v.toFixed(3), ends: ['More results', 'Closer matches'] },
      ],
    },
    {
      id: 'dupes',
      title: 'Duplicates',
      blurb: 'The Strict, Normal and Loose levels of the Duplicates report.',
      fields: [
        { k: 'near_strict', label: 'Near: strict', hint: 'Perceptual-hash bits apart (of 64).', kind: 'num', min: 0, max: 20, step: 1 },
        { k: 'near_normal', label: 'Near: normal', hint: 'Perceptual-hash bits apart.', kind: 'num', min: 0, max: 24, step: 1 },
        { k: 'near_loose', label: 'Near: loose', hint: 'Perceptual-hash bits apart.', kind: 'num', min: 0, max: 28, step: 1 },
        { k: 'alike_strict', label: 'Look-alike: strict', hint: 'Visual likeness (cosine).', kind: 'range', min: 0.7, max: 0.999, step: 0.005, fmt: cos },
        { k: 'alike_normal', label: 'Look-alike: normal', hint: 'Visual likeness (cosine).', kind: 'range', min: 0.7, max: 0.999, step: 0.005, fmt: cos },
        { k: 'alike_loose', label: 'Look-alike: loose', hint: 'Visual likeness (cosine).', kind: 'range', min: 0.7, max: 0.999, step: 0.005, fmt: cos },
      ],
    },
  ];

  let draft = $state<Tuning | null>(tuning.values ? { ...tuning.values } : null);
  /** The values the draft started from. */
  let base: Tuning | null = tuning.values;
  let saving = $state(false);
  let error = $state('');

  // Loaded late, or changed elsewhere while nothing here is edited: follow it.
  $effect(() => {
    const v = tuning.values;
    if (!v) return;
    untrack(() => {
      const edited = draft && base && (Object.keys(base) as Key[]).some((k) => base![k] !== draft![k]);
      if (!edited) draft = { ...v };
      base = v;
    });
  });
  if (!tuning.values) tuning.load();

  const defaults = $derived(tuning.defaults);

  function dirtyKeys(): Key[] {
    const v = tuning.values;
    if (!v || !draft) return [];
    return (Object.keys(v) as Key[]).filter((k) => v[k] !== draft![k]);
  }
  const dirty = $derived(dirtyKeys());

  function setv(k: Key, v: number | boolean) {
    if (!draft) return;
    draft = { ...draft, [k]: v };
  }

  async function apply() {
    if (!draft || !dirty.length) return;
    saving = true;
    error = '';
    const patch: Partial<Tuning> = {};
    for (const k of dirty) (patch as Record<string, unknown>)[k] = draft[k];
    try {
      const t = await api.setTuning(patch);
      tuning.set(t);
      draft = { ...t.values };
      store.toast('Advanced settings applied', 'ok', undefined, 2200);
    } catch (e) {
      error = (e as Error).message;
    }
    saving = false;
  }

  async function resetAll() {
    saving = true;
    error = '';
    try {
      const t = await api.resetTuning();
      tuning.set(t);
      draft = { ...t.values };
      store.toast('Advanced settings back to their defaults', 'ok', undefined, 2200);
    } catch (e) {
      error = (e as Error).message;
    }
    saving = false;
  }

  const isDefault = $derived(!!defaults && !!draft && (Object.keys(defaults) as Key[]).every((k) => defaults![k] === draft![k]));

  // ---------------------------------------------------------------- series preview
  const root = store.roots[0]?.id;
  const openFolder = store.folderId != null && store.folderId !== root ? store.folders.get(store.folderId) : undefined;
  let scope = $state<'folder' | 'library'>(openFolder ? 'folder' : 'library');
  let preview = $state.raw<{ now: SeriesStats; candidate: SeriesStats } | null>(null);
  let previewing = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let ctl: AbortController | null = null;

  $effect(() => {
    const d = draft;
    const sc = scope;
    if (!d) return;
    const p = {
      folder: sc === 'folder' ? openFolder?.id : undefined,
      burst: d.series_burst,
      gap: d.series_gap,
      strictness: d.series_strictness,
      scene: d.series_scene,
      min: d.series_min,
      cameras: d.series_cameras,
    };
    if (timer) clearTimeout(timer);
    timer = setTimeout(async () => {
      ctl?.abort();
      const c = (ctl = new AbortController());
      previewing = true;
      try {
        const r = await api.previewSeries(p, c.signal);
        if (!c.signal.aborted) preview = r;
      } catch {
        /* aborted or offline: keep the last numbers */
      }
      if (!c.signal.aborted) previewing = false;
    }, 220);
  });
  onDestroy(() => {
    if (timer) clearTimeout(timer);
    ctl?.abort();
  });

  const delta = (a: number, b: number) => (b === a ? '' : b > a ? `+${(b - a).toLocaleString()}` : `−${(a - b).toLocaleString()}`);
  const singles = (s: SeriesStats) => s.photos - s.in_series;

  const fmtv = (f: Field, v: number | boolean) => (typeof v === 'boolean' ? (v ? 'On' : 'Off') : f.fmt ? f.fmt(v) : String(v));
</script>

{#if !draft}
  <div class="center"><div class="spinner"></div></div>
{:else}
  <div class="adv">
    <p class="lede dim">These apply to the whole library and to everyone using it. The defaults were calibrated on real shoots; change one at a time and watch the preview.</p>

    {#each groups as g (g.id)}
      <section aria-labelledby={`adv-${g.id}`}>
        <h4 id={`adv-${g.id}`}>{g.title}</h4>
        <p class="blurb dim">{g.blurb}</p>

        {#if g.id === 'series'}
          <div class="preview" class:busy={previewing} aria-live="polite">
            <div class="ph">
              <span class="pt">Preview</span>
              {#if openFolder}
                <div class="seg sm" role="radiogroup" aria-label="Preview of">
                  <button role="radio" aria-checked={scope === 'folder'} class:on={scope === 'folder'} onclick={() => (scope = 'folder')} title={openFolder.name}>This folder</button>
                  <button role="radio" aria-checked={scope === 'library'} class:on={scope === 'library'} onclick={() => (scope = 'library')}>Library</button>
                </div>
              {:else}
                <span class="dim">Whole library</span>
              {/if}
            </div>
            {#if preview}
              {@const n = preview.now}
              {@const c = preview.candidate}
              <div class="stats">
                <div><b class="tnum">{c.series.toLocaleString()}</b><span>series</span>{#if delta(n.series, c.series)}<em class="tnum">{delta(n.series, c.series)}</em>{/if}</div>
                <div><b class="tnum">{c.in_series.toLocaleString()}</b><span>photos in series</span>{#if delta(n.in_series, c.in_series)}<em class="tnum">{delta(n.in_series, c.in_series)}</em>{/if}</div>
                <div><b class="tnum">{singles(c).toLocaleString()}</b><span>single photos</span>{#if delta(singles(n), singles(c))}<em class="tnum">{delta(singles(n), singles(c))}</em>{/if}</div>
                <div><b class="tnum">{c.largest.toLocaleString()}</b><span>largest series</span></div>
              </div>
              {#if !c.photos}<p class="dim small">No photos with a capture time here.</p>{/if}
            {:else}
              <div class="stats skeleton"><div></div><div></div><div></div><div></div></div>
            {/if}
          </div>
        {/if}

        {#each g.fields as f (f.k)}
          {@const v = draft[f.k]}
          {@const changed = tuning.values ? tuning.values[f.k] !== v : false}
          {@const custom = defaults ? defaults[f.k] !== v : false}
          <div class="field-row" class:changed>
            <div class="lab">
              <label for={`t-${f.k}`}>{f.label}</label>
              <span class="hint dim">{f.hint}</span>
            </div>
            <div class="ctl">
              {#if f.kind === 'switch'}
                <span class="switch"><input id={`t-${f.k}`} type="checkbox" checked={v as boolean} onchange={(e) => setv(f.k, (e.target as HTMLInputElement).checked)} /><span></span></span>
              {:else if f.kind === 'num'}
                <input id={`t-${f.k}`} class="field num tnum" type="number" min={f.min} max={f.max} step={f.step} value={v} onchange={(e) => { const x = +(e.target as HTMLInputElement).value; if (Number.isFinite(x)) setv(f.k, Math.min(f.max ?? x, Math.max(f.min ?? x, Math.round(x)))); }} />
              {:else}
                <div class="slide">
                  <input id={`t-${f.k}`} class="range" type="range" min={f.min} max={f.max} step={f.step} value={v} oninput={(e) => setv(f.k, +(e.target as HTMLInputElement).value)} aria-valuetext={fmtv(f, v)} />
                  {#if f.ends}<div class="ends dim"><span>{f.ends[0]}</span><span>{f.ends[1]}</span></div>{/if}
                </div>
                <output class="val tnum" for={`t-${f.k}`}>{fmtv(f, v)}</output>
              {/if}
              <button class="btn ghost icon xs undo" class:hide={!custom} onclick={() => defaults && setv(f.k, defaults[f.k])} title={defaults ? `Default: ${fmtv(f, defaults[f.k])}` : 'Default'} aria-label={`${f.label}: back to the default`} tabindex={custom ? 0 : -1}><Icon name="undo" size={13} /></button>
            </div>
          </div>
        {/each}
      </section>
    {/each}
  </div>

  <div class="bar">
    <button class="btn ghost sm" onclick={resetAll} disabled={saving || isDefault}>Reset all to defaults</button>
    <span class="grow">{#if error}<span class="err">{error}</span>{:else if dirty.length}<span class="dim">{dirty.length} change{dirty.length > 1 ? 's' : ''} not applied</span>{/if}</span>
    <button class="btn sm" onclick={() => tuning.values && (draft = { ...tuning.values })} disabled={saving || !dirty.length}>Discard</button>
    <button class="btn primary sm" onclick={apply} disabled={saving || !dirty.length}>Apply</button>
  </div>
{/if}

<style>
  .adv {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    padding-bottom: 0.5rem;
  }
  .lede {
    margin: 0;
    font-size: var(--fs-sm);
  }
  h4 {
    margin: 0;
    font-size: var(--fs);
    font-weight: 650;
  }
  .blurb {
    margin: 0.125rem 0 0.5rem;
    font-size: var(--fs-sm);
  }
  .field-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(12rem, 15rem);
    gap: 0.375rem 1.25rem;
    align-items: center;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .field-row:last-child {
    border-bottom: 0;
  }
  .lab {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    min-width: 0;
  }
  .lab label {
    font-weight: 550;
  }
  .field-row.changed .lab label::after {
    content: '';
    display: inline-block;
    width: 0.375rem;
    height: 0.375rem;
    margin-left: 0.4375rem;
    border-radius: 50%;
    background: var(--accent);
    vertical-align: middle;
  }
  .hint {
    font-size: var(--fs-xs);
    line-height: 1.4;
  }
  .ctl {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .slide {
    flex: 1;
    min-width: 0;
  }
  .ends {
    display: flex;
    justify-content: space-between;
    font-size: var(--fs-xs);
    margin-top: -0.125rem;
  }
  .val {
    min-width: 4.25rem;
    text-align: right;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .num {
    width: 4.5rem;
    height: 1.875rem;
    text-align: right;
  }
  .undo.hide {
    visibility: hidden;
  }
  .xs {
    width: 1.5rem;
    height: 1.5rem;
    min-width: 1.5rem;
    padding: 0;
  }
  .range {
    width: 100%;
    height: 1.25rem;
    appearance: none;
    -webkit-appearance: none;
    background: transparent;
    cursor: pointer;
    display: block;
  }
  .range::-webkit-slider-runnable-track {
    height: 3px;
    border-radius: 3px;
    background: var(--surface-3);
  }
  .range::-moz-range-track {
    height: 3px;
    border-radius: 3px;
    background: var(--surface-3);
  }
  .range::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 0.875rem;
    height: 0.875rem;
    margin-top: -0.3125rem;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.4);
  }
  .range::-moz-range-thumb {
    width: 0.875rem;
    height: 0.875rem;
    border: 0;
    border-radius: 50%;
    background: var(--accent);
  }
  .range:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
    border-radius: 4px;
  }
  .preview {
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface-2);
    padding: 0.625rem 0.75rem 0.75rem;
    margin-bottom: 0.25rem;
    transition: opacity var(--dur);
  }
  .preview.busy {
    opacity: 0.7;
  }
  .ph {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
    font-size: var(--fs-sm);
  }
  .pt {
    font-weight: 600;
    color: var(--text-2);
  }
  .seg.sm > button {
    height: 1.625rem;
    font-size: var(--fs-xs);
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 0.5rem;
  }
  .stats > div {
    display: flex;
    flex-direction: column;
    gap: 0.0625rem;
    min-width: 0;
  }
  .stats b {
    font-size: var(--fs-xl);
    font-weight: 650;
    line-height: 1.1;
  }
  .stats span {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .stats em {
    font-style: normal;
    font-size: var(--fs-xs);
    color: var(--accent);
    font-weight: 600;
  }
  .stats.skeleton > div {
    height: 2.5rem;
    border-radius: var(--r);
    background: var(--surface-3);
    opacity: 0.5;
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0.5rem 0 0;
  }
  .bar {
    position: sticky;
    bottom: -1rem;
    margin: 0 -1.25rem -1rem;
    padding: 0.625rem 1.25rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--surface);
    border-top: 1px solid var(--border);
    z-index: 1;
  }
  .grow {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    color: var(--negative);
  }
  .center {
    display: grid;
    place-items: center;
    padding: 3rem;
  }
  @media (max-width: 760px) {
    .field-row {
      grid-template-columns: 1fr;
    }
    .stats {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .bar {
      flex-wrap: wrap;
    }
  }
</style>
