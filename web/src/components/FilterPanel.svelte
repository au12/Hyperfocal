<script lang="ts">
  // The filters of the photo grid and the map: one compact panel, a label column on the
  // left and the controls on the right, grouped (the photo, the file, people, culling).
  // The header and the footer stay put while the body scrolls; the footer says how many
  // photos are shown, live.
  import { store, LABELS, activeFilterCount, type Filters } from '../lib/store.svelte';
  import { api, type Facets } from '../lib/api';
  import { ISSUES } from '../lib/cull.svelte';
  import { segmented } from '../lib/motion';
  import Icon from './Icon.svelte';

  let { onMap = false, count, close }: { onMap?: boolean; count: number; close: () => void } = $props();

  let facets = $state<Facets | null>(null);
  api
    .facets()
    .then((x) => (facets = x))
    .catch(() => {});

  const f = $derived(store.filters);
  const active = $derived(activeFilterCount(f) - (onMap && f.gps === 1 ? 1 : 0));
  /** The star under the pointer, for a preview of the minimum. */
  let hover = $state(0);

  function set<K extends keyof Filters>(k: K, v: Filters[K]) {
    store.setFilter(k, v);
  }

  function setRating(r: number) {
    store.filters.unrated = false;
    set('rmin', f.rmin === r ? 0 : r);
  }

  function toggleLabel(c: string) {
    set('lc', f.lc.includes(c) ? f.lc.filter((x) => x !== c) : [...f.lc, c]);
  }

  function toggleIssue(k: string) {
    const cur = f.issue ?? [];
    set('issue', cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]);
  }

  function rejected(v: 'hide' | 'show' | 'only') {
    store.filters.onlyRejected = v === 'only';
    set('showRejected', v !== 'hide');
  }

  function reset() {
    Object.assign(store.filters, {
      rmin: 0, unrated: false, lc: [], pick: null, kinds: [], gps: onMap ? 1 : null, cam: '', lens: '', kw: '', onlyRejected: false,
      dateFrom: '', dateTo: '', subj: [], ids: [], ppl: '', shot: '', eyes: '', issue: [], weak: false,
    });
    store.persist();
    store.reload();
  }

  const kindsKey = $derived(!f.kinds.length ? 'all' : f.kinds.length > 1 ? 'other' : String(f.kinds[0]));
  const rej = $derived(f.onlyRejected ? 'only' : f.showRejected ? 'show' : 'hide');
  const shown = $derived(hover || f.rmin);
</script>

<div class="fp">
  <header class="fp-head">
    <Icon name="filter" size={15} />
    <span class="title">Filters</span>
    {#if active > 0}<span class="badge">{active}</span>{/if}
    <span class="grow"></span>
    <button class="btn sm ghost" onclick={reset} disabled={active === 0} title="Show every photo again">Reset</button>
  </header>

  <div class="fp-body">
    <section>
      <h4>Photo</h4>
      <div class="r">
        <span class="lab">Rating</span>
        <div class="ctl">
          <div class="stars" role="group" aria-label="Minimum rating" onpointerleave={() => (hover = 0)}>
            {#each [1, 2, 3, 4, 5] as r (r)}
              <button
                class="star"
                class:on={!f.unrated && r <= shown}
                onpointerenter={() => (hover = r)}
                onclick={() => setRating(r)}
                aria-pressed={!f.unrated && f.rmin === r}
                title={f.rmin === r ? 'Any rating' : `${r} star${r > 1 ? 's' : ''}${r < 5 ? ' or more' : ''}`}
              >
                <Icon name="star" size={16} fill={!f.unrated && r <= shown} />
              </button>
            {/each}
            <span class="hint tnum">{f.unrated ? '' : shown ? `${shown}★${shown < 5 ? '+' : ''}` : 'Any'}</span>
          </div>
          <button class="chip-btn" class:on={f.unrated} onclick={() => { store.filters.rmin = 0; set('unrated', !f.unrated); }}>Unrated</button>
        </div>
      </div>
      <div class="r">
        <span class="lab">Flag</span>
        <div class="seg small" use:segmented>
          <button class:on={f.pick == null} onclick={() => set('pick', null)}>All</button>
          <button class:on={f.pick === 1} onclick={() => set('pick', 1)}>Picks</button>
          <button class:on={f.pick === 0} onclick={() => set('pick', 0)}>Unflagged</button>
        </div>
      </div>
      <div class="r">
        <span class="lab">Label</span>
        <div class="ctl">
          {#each LABELS as l (l.color)}
            <button class="dot" class:on={f.lc.includes(l.color)} onclick={() => toggleLabel(l.color)} title={l.name} aria-label={l.name} aria-pressed={f.lc.includes(l.color)}>
              <span class="label-dot label-{l.color}"></span>
            </button>
          {/each}
          <button class="chip-btn" class:on={f.lc.includes('none')} onclick={() => toggleLabel('none')}>None</button>
        </div>
      </div>
      <div class="r">
        <span class="lab">Rejected</span>
        <div class="seg small" use:segmented title="Photos in the reject folders">
          <button class:on={rej === 'hide'} onclick={() => rejected('hide')}>Hide</button>
          <button class:on={rej === 'show'} onclick={() => rejected('show')}>Show</button>
          <button class:on={rej === 'only'} onclick={() => rejected('only')}>Only</button>
        </div>
      </div>
    </section>

    <section>
      <h4>File</h4>
      <div class="r">
        <span class="lab">Type</span>
        <div class="seg small" use:segmented>
          <button class:on={kindsKey === 'all'} onclick={() => set('kinds', [])}>All</button>
          <button class:on={kindsKey === '1'} onclick={() => set('kinds', [1])}>RAW</button>
          <button class:on={kindsKey === '2'} onclick={() => set('kinds', [2])}>JPEG</button>
          <button class:on={kindsKey === '5'} onclick={() => set('kinds', [5])}>HEIC</button>
          <button class:on={kindsKey === 'other'} onclick={() => set('kinds', [3, 4])} title="TIFF and PNG">Other</button>
        </div>
      </div>
      <div class="r">
        <span class="lab">Taken</span>
        <div class="dates">
          <input class="field" type="date" value={f.dateFrom} max={f.dateTo || undefined} onchange={(e) => set('dateFrom', (e.target as HTMLInputElement).value)} aria-label="From" />
          <span class="dim">–</span>
          <input class="field" type="date" value={f.dateTo} min={f.dateFrom || undefined} onchange={(e) => set('dateTo', (e.target as HTMLInputElement).value)} aria-label="To" />
        </div>
      </div>
      <div class="r">
        <span class="lab">Camera</span>
        <select class="field" value={f.cam} onchange={(e) => set('cam', (e.target as HTMLSelectElement).value)}>
          <option value="">Any camera</option>
          {#each facets?.cameras ?? [] as [name, n] (name)}<option value={name}>{name} · {n.toLocaleString()}</option>{/each}
          {#if f.cam && !facets?.cameras.some((c) => c[0] === f.cam)}<option value={f.cam}>{f.cam}</option>{/if}
        </select>
      </div>
      <div class="r">
        <span class="lab">Lens</span>
        <select class="field" value={f.lens} onchange={(e) => set('lens', (e.target as HTMLSelectElement).value)}>
          <option value="">Any lens</option>
          {#each facets?.lenses ?? [] as [name, n] (name)}<option value={name}>{name} · {n.toLocaleString()}</option>{/each}
          {#if f.lens && !facets?.lenses.some((c) => c[0] === f.lens)}<option value={f.lens}>{f.lens}</option>{/if}
        </select>
      </div>
      {#if !onMap}
        <div class="r">
          <span class="lab">Location</span>
          <div class="seg small" use:segmented>
            <button class:on={f.gps == null} onclick={() => set('gps', null)}>All</button>
            <button class:on={f.gps === 1} onclick={() => set('gps', 1)}>With GPS</button>
            <button class:on={f.gps === 0} onclick={() => set('gps', 0)}>Without</button>
          </div>
        </div>
      {/if}
      {#if f.kw}
        <div class="r">
          <span class="lab">Keyword</span>
          <div class="ctl"><span class="chip kw"><span class="ellipsis">{f.kw.split('|').join(' › ')}</span><button onclick={() => set('kw', '')} aria-label="Remove the keyword"><Icon name="x" size={12} /></button></span></div>
        </div>
      {/if}
    </section>

    <section>
      <h4>People</h4>
      <div class="r">
        <span class="lab">How many</span>
        <div class="seg small" use:segmented>
          {#each [['', 'Any'], ['0', 'None'], ['1', '1'], ['2', '2'], ['3-5', '3–5'], ['6-', '6+']] as [v, t] (v)}
            <button class:on={(f.ppl ?? '') === v} onclick={() => set('ppl', v)}>{t}</button>
          {/each}
        </div>
      </div>
      <div class="r">
        <span class="lab">Shot</span>
        <div class="seg small" use:segmented>
          <button class:on={!f.shot} onclick={() => set('shot', '')}>Any</button>
          <button class:on={f.shot === 'portrait'} onclick={() => set('shot', 'portrait')} title="One person, close">Portraits</button>
          <button class:on={f.shot === 'group'} onclick={() => set('shot', 'group')} title="Three people or more">Groups</button>
        </div>
      </div>
      <div class="r">
        <span class="lab">Eyes</span>
        <div class="seg small" use:segmented>
          <button class:on={!f.eyes} onclick={() => set('eyes', '')}>Any</button>
          <button class:on={f.eyes === 'open'} onclick={() => set('eyes', 'open')} title="Every face looked at has its eyes open">Open</button>
          <button class:on={f.eyes === 'closed'} onclick={() => set('eyes', 'closed')} title="Someone's eyes are closed">Closed</button>
        </div>
      </div>
    </section>

    <section>
      <h4>Culling <span class="sub">possible rejects, any of</span></h4>
      <div class="issues">
        {#each ISSUES as is (is.key)}
          <button class="chip-btn issue {is.key}" class:on={(f.issue ?? []).includes(is.key)} onclick={() => toggleIssue(is.key)} title={is.hint}><Icon name={is.icon} size={13} />{is.label}</button>
        {/each}
        <button class="chip-btn issue weak" class:on={!!f.weak} onclick={() => set('weak', !f.weak)} title="Every frame of a series but its best (the one you rated or picked, else open eyes and the sharpest)"><Icon name="stack" size={13} />Weaker frames</button>
      </div>
    </section>
  </div>

  <footer class="fp-foot">
    <span class="dim tnum">{count.toLocaleString()} {count === 1 ? 'photo' : 'photos'}</span>
    <span class="grow"></span>
    <button class="btn sm primary" onclick={close}>Done</button>
  </footer>
</div>

<style>
  .fp {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .fp-head,
  .fp-foot {
    position: sticky;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--raised);
  }
  .fp-head {
    top: 0;
    padding: 0.625rem 0.625rem 0.5rem 0.875rem;
    border-bottom: 1px solid var(--border);
  }
  .fp-head :global(svg) {
    color: var(--accent);
  }
  .title {
    font-weight: 680;
    font-size: var(--fs-lg);
    letter-spacing: -0.01em;
  }
  .fp-foot {
    bottom: 0;
    padding: 0.5rem 0.625rem 0.5rem 0.875rem;
    border-top: 1px solid var(--border);
  }
  .fp-body {
    padding: 0.25rem 0.875rem 0.375rem;
  }
  section + section {
    border-top: 1px solid var(--border);
  }
  section {
    padding: 0.5rem 0 0.625rem;
  }
  h4 {
    margin: 0.125rem 0 0.375rem;
    font-size: var(--fs-xs);
    font-weight: 650;
    color: var(--text-3);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  h4 .sub {
    font-weight: 450;
    text-transform: none;
    letter-spacing: 0;
    margin-left: 0.25rem;
  }
  /* A row: the label, then the control. */
  .r {
    display: grid;
    grid-template-columns: 4.75rem minmax(0, 1fr);
    align-items: center;
    gap: 0.5rem;
    min-height: 2.125rem;
  }
  .lab {
    color: var(--text-2);
    font-size: var(--fs-sm);
    font-weight: 550;
  }
  .ctl {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.3125rem;
    min-width: 0;
  }
  .seg.small {
    display: flex;
    min-width: 0;
  }
  .seg.small > button {
    flex: 1;
    height: 1.75rem;
    padding: 0 0.375rem;
    font-size: var(--fs-sm);
    min-width: 0;
    white-space: nowrap;
  }
  .stars {
    display: inline-flex;
    align-items: center;
    margin-right: 0.25rem;
  }
  .star {
    width: 1.5rem;
    height: 1.75rem;
    display: grid;
    place-items: center;
    border: 0;
    background: none;
    padding: 0;
    color: var(--text-3);
    cursor: pointer;
    transition:
      color var(--dur-fast),
      transform var(--dur-fast) var(--ease-spring);
  }
  .star:hover {
    transform: scale(1.15);
  }
  .star.on {
    color: var(--star);
  }
  .stars .hint {
    margin-left: 0.375rem;
    min-width: 2rem;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .chip-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.3125rem;
    height: 1.75rem;
    padding: 0 0.625rem;
    border: 1px solid var(--border-strong);
    border-radius: 99px;
    background: var(--surface);
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-sm);
    font-weight: 560;
    cursor: pointer;
    white-space: nowrap;
    transition:
      background var(--dur-fast),
      color var(--dur-fast),
      border-color var(--dur-fast);
  }
  .chip-btn:hover {
    color: var(--text);
  }
  .chip-btn.on {
    background: color-mix(in srgb, var(--chip, var(--accent)) 18%, transparent);
    border-color: color-mix(in srgb, var(--chip, var(--accent)) 60%, transparent);
    color: var(--text);
  }
  .chip-btn :global(svg) {
    color: var(--chip, var(--text-3));
  }
  .dot {
    width: 1.75rem;
    height: 1.75rem;
    display: grid;
    place-items: center;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--surface);
    cursor: pointer;
    padding: 0;
    transition:
      transform var(--dur-fast) var(--ease-spring),
      box-shadow var(--dur);
  }
  .dot:hover {
    transform: scale(1.08);
  }
  .dot.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent);
  }
  .dates {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    min-width: 0;
  }
  .dates .field {
    flex: 1;
    min-width: 0;
    height: 1.875rem;
    font-size: var(--fs-sm);
    padding: 0 0.375rem;
  }
  select.field {
    width: 100%;
    min-width: 0;
    height: 1.875rem;
    font-size: var(--fs-sm);
  }
  .chip.kw {
    max-width: 100%;
  }
  .issues {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3125rem;
  }
  .issue.blur {
    --chip: #f59e0b;
  }
  .issue.eyes {
    --chip: #ff5c61;
  }
  .issue.over {
    --chip: #fb7a3c;
  }
  .issue.under {
    --chip: #6b8cff;
  }
  .issue.weak {
    --chip: var(--accent);
  }
  /* A phone: a little more room for fingers. */
  :global(.sheet) .r {
    min-height: 2.5rem;
  }
  :global(.sheet) .seg.small > button,
  :global(.sheet) .chip-btn,
  :global(.sheet) .dot {
    height: 2rem;
  }
  :global(.sheet) .dot {
    width: 2rem;
  }
</style>
