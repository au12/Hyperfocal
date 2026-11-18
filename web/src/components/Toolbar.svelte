<script lang="ts">
  import { store, LABELS, activeFilterCount, type SortKey } from '../lib/store.svelte';
  import { api, type Facets } from '../lib/api';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';

  let facets = $state<Facets | null>(null);
  let filterOpen = $state(false);
  let saveOpen = $state(false);
  let saveName = $state('');

  const crumbs = $derived(store.folderPath(store.folderId));
  const f = $derived(store.filters);
  const count = $derived(store.list.id.length);
  const activeFilters = $derived(activeFilterCount(f));
  const coll = $derived(store.collection);
  const modified = $derived(store.collectionModified());
  /** Something worth saving: a search, a period or a filter. */
  const saveable = $derived(!!store.query.trim() || !!store.dateRange || activeFilters > 0 || (coll != null && modified));

  async function loadFacets() {
    if (!facets) facets = await api.facets().catch(() => null);
  }

  function toggleLabel(c: string) {
    const lc = f.lc.includes(c) ? f.lc.filter((x) => x !== c) : [...f.lc, c];
    store.setFilter('lc', lc);
  }

  function clearFilters() {
    Object.assign(store.filters, { rmin: 0, unrated: false, lc: [], pick: null, kinds: [], gps: null, cam: '', lens: '', kw: '', onlyRejected: false, dateFrom: '', dateTo: '' });
    store.persist();
    store.reload();
  }

  /** A name describing the view, as a starting point. */
  function suggestName(): string {
    const parts: string[] = [];
    if (store.query.trim()) parts.push(`“${store.query.trim()}”`);
    if (f.rmin > 0) parts.push(`${f.rmin}★${f.rmin < 5 ? '+' : ''}`);
    if (f.unrated) parts.push('Unrated');
    if (f.pick === 1) parts.push('Picks');
    if (f.pick === 0) parts.push('Unflagged');
    for (const c of f.lc) parts.push(c === 'none' ? 'No label' : (LABELS.find((l) => l.color === c)?.name ?? c));
    if (f.kw) parts.push(f.kw.split('|').pop() ?? f.kw);
    if (f.cam) parts.push(f.cam);
    if (f.lens) parts.push(f.lens);
    if (f.gps === 0) parts.push('No location');
    if (f.gps === 1) parts.push('With location');
    if (store.dateRange) parts.push(store.dateRange.label);
    if (f.dateFrom || f.dateTo) parts.push([f.dateFrom, f.dateTo].filter(Boolean).join(' – '));
    if (f.onlyRejected) parts.push('Rejected');
    const folder = store.folders.get(store.folderId ?? -1);
    if (folder && folder.path && !store.dateRange && !(store.query.trim() && !coll)) parts.push(folder.name);
    return parts.join(' · ').slice(0, 120) || 'New collection';
  }

  function openSave(toggle: () => void) {
    saveName = coll && typeof coll.id === 'number' ? coll.name : suggestName();
    toggle();
  }

  async function save(asNew: boolean, close: () => void) {
    const name = saveName.trim();
    if (!name) return;
    close();
    await store.saveCollection(name, !asNew && coll && typeof coll.id === 'number' ? coll.id : undefined);
  }

  function setDate(k: 'dateFrom' | 'dateTo', v: string) {
    store.setFilter(k, v);
  }

  const sorts: [SortKey, string][] = [
    ['taken', 'Capture time'],
    ['name', 'File name'],
    ['rating', 'Rating'],
    ['modified', 'File date'],
  ];

  function setRow(e: Event) {
    store.settings.rowHeight = +(e.target as HTMLInputElement).value;
    store.persist();
  }
</script>

<div class="toolbar">
  <div class="crumbs">
    {#if coll}
      <span class="crumb current" title="Collection"><Icon name={typeof coll.id === 'number' ? 'bookmark' : 'library'} size={14} /> {coll.name}</span>
      {#if modified}<span class="modified" title="Filters or search differ from the saved collection">modified</span>{/if}
    {:else if store.query}
      {#if store.dateRange}<button class="crumb" onclick={() => store.search('')}>{store.dateRange.label}</button><Icon name="chevron" size={14} class="sep" />{/if}
      <span class="crumb current"><Icon name="search" size={14} /> “{store.query}”</span>
      <button class="btn sm ghost" onclick={() => store.search('')}>Clear</button>
    {:else if store.dateRange}
      <span class="crumb"><Icon name="calendar" size={14} /> Timeline</span>
      <Icon name="chevron" size={14} class="sep" />
      <span class="crumb current">{store.dateRange.label}</span>
    {:else}
      {#each crumbs as c, i (c.id)}
        {#if i > 0}<Icon name="chevron" size={14} class="sep" />{/if}
        <button class="crumb" class:current={i === crumbs.length - 1} onclick={() => store.openFolder(c.id)}>
          {i === 0 ? 'Library' : c.name}
        </button>
      {/each}
    {/if}
    <span class="count tnum">{count.toLocaleString()} {count === 1 ? 'photo' : 'photos'}</span>
    {#if store.loading}<span class="spinner"></span>{/if}
  </div>

  <div class="tools">
    {#if !store.query && store.mobile}
      <button class="btn sm icon" class:on={store.recursive} onclick={() => { store.recursive = !store.recursive; store.reload(); }} title="Include sub folders" aria-label="Include sub folders"><Icon name="layers" /></button>
    {/if}
    {#if !store.query && !store.mobile}
      <label class="check" title="Include photos in sub folders">
        <input type="checkbox" checked={store.recursive} onchange={(e) => { store.recursive = (e.target as HTMLInputElement).checked; store.reload(); }} />
        Sub folders
      </label>
    {/if}

    <Popover bind:open={filterOpen} align="right" width="19rem">
      {#snippet trigger(toggle, open)}
        <button class="btn sm" class:on={open || activeFilters > 0} onclick={() => { toggle(); loadFacets(); }}>
          <Icon name="filter" /> {store.mobile ? '' : 'Filter'}
          {#if activeFilters > 0}<span class="badge">{activeFilters}</span>{/if}
        </button>
      {/snippet}
      {#snippet children()}
        <div class="filters">
          <div class="fl">Rating</div>
          <div class="stars">
            {#each [0, 1, 2, 3, 4, 5] as r}
              <button class="btn sm" class:on={!f.unrated && f.rmin === r} onclick={() => { store.filters.unrated = false; store.setFilter('rmin', r); }}>
                {r === 0 ? 'Any' : `≥ ${r}★`}
              </button>
            {/each}
            <button class="btn sm" class:on={f.unrated} onclick={() => { store.filters.rmin = 0; store.setFilter('unrated', !f.unrated); }}>Unrated</button>
          </div>
          <div class="fl">Label</div>
          <div class="row wrap">
            {#each LABELS as l}
              <button class="lbl" class:on={f.lc.includes(l.color)} onclick={() => toggleLabel(l.color)} title={l.name}>
                <span class="label-dot label-{l.color}"></span>
              </button>
            {/each}
            <button class="btn sm" class:on={f.lc.includes('none')} onclick={() => toggleLabel('none')}>None</button>
          </div>
          <div class="fl">Flag</div>
          <div class="seg small">
            <button class:on={f.pick == null} onclick={() => store.setFilter('pick', null)}>All</button>
            <button class:on={f.pick === 1} onclick={() => store.setFilter('pick', 1)}>Picks</button>
            <button class:on={f.pick === 0} onclick={() => store.setFilter('pick', 0)}>Unflagged</button>
          </div>
          <div class="fl">File type</div>
          <div class="seg small">
            <button class:on={!f.kinds.length} onclick={() => store.setFilter('kinds', [])}>All</button>
            <button class:on={f.kinds.length === 1 && f.kinds[0] === 1} onclick={() => store.setFilter('kinds', [1])}>RAW</button>
            <button class:on={f.kinds.length === 1 && f.kinds[0] === 2} onclick={() => store.setFilter('kinds', [2])}>JPEG</button>
            <button class:on={f.kinds.length === 1 && f.kinds[0] === 5} onclick={() => store.setFilter('kinds', [5])}>HEIC</button>
            <button class:on={f.kinds.length > 1} onclick={() => store.setFilter('kinds', [3, 4])}>Other</button>
          </div>
          <div class="fl">Capture date</div>
          <div class="dates">
            <input class="field" type="date" value={f.dateFrom} max={f.dateTo || undefined} onchange={(e) => setDate('dateFrom', (e.target as HTMLInputElement).value)} aria-label="From" />
            <span class="dim">to</span>
            <input class="field" type="date" value={f.dateTo} min={f.dateFrom || undefined} onchange={(e) => setDate('dateTo', (e.target as HTMLInputElement).value)} aria-label="To" />
          </div>
          <div class="fl">Location</div>
          <div class="seg small">
            <button class:on={f.gps == null} onclick={() => store.setFilter('gps', null)}>All</button>
            <button class:on={f.gps === 1} onclick={() => store.setFilter('gps', 1)}>With GPS</button>
            <button class:on={f.gps === 0} onclick={() => store.setFilter('gps', 0)}>Without</button>
          </div>
          <div class="fl">Camera & lens</div>
          <select class="field" value={f.cam} onchange={(e) => store.setFilter('cam', (e.target as HTMLSelectElement).value)}>
            <option value="">All cameras</option>
            {#each facets?.cameras ?? [] as [name, n]}<option value={name}>{name} ({n})</option>{/each}
          </select>
          <select class="field" value={f.lens} onchange={(e) => store.setFilter('lens', (e.target as HTMLSelectElement).value)}>
            <option value="">All lenses</option>
            {#each facets?.lenses ?? [] as [name, n]}<option value={name}>{name} ({n})</option>{/each}
          </select>
          {#if f.kw}
            <div class="fl">Keyword</div>
            <span class="chip">{f.kw}<button onclick={() => store.setFilter('kw', '')} aria-label="Remove"><Icon name="x" size={12} /></button></span>
          {/if}
          <div class="fl">Rejected photos</div>
          <div class="seg small">
            <button class:on={!f.showRejected && !f.onlyRejected} onclick={() => { store.filters.onlyRejected = false; store.setFilter('showRejected', false); }}>Hide</button>
            <button class:on={f.showRejected && !f.onlyRejected} onclick={() => { store.filters.onlyRejected = false; store.setFilter('showRejected', true); }}>Show</button>
            <button class:on={f.onlyRejected} onclick={() => { store.filters.showRejected = true; store.setFilter('onlyRejected', true); }}>Only</button>
          </div>
          <div class="foot">
            <button class="btn sm ghost" onclick={clearFilters} disabled={activeFilters === 0}>Reset filters</button>
          </div>
        </div>
      {/snippet}
    </Popover>

    {#if saveable}
      <Popover bind:open={saveOpen} align="right" width="18rem">
        {#snippet trigger(toggle, open)}
          <button class="btn sm" class:on={open} onclick={() => openSave(toggle)} title="Save this view as a collection">
            <Icon name="bookmark-plus" />{store.mobile ? '' : coll && typeof coll.id === 'number' && modified ? 'Update' : 'Save'}
          </button>
        {/snippet}
        {#snippet children(close)}
          <form class="savebox" onsubmit={(e) => { e.preventDefault(); save(!(coll && typeof coll.id === 'number'), close); }}>
            <div class="fl">Save the search, filters and dates as a collection</div>
            <!-- svelte-ignore a11y_autofocus -->
            <input class="field" bind:value={saveName} maxlength="120" autofocus placeholder="Name" />
            <div class="row end">
              {#if coll && typeof coll.id === 'number'}
                <button type="button" class="btn sm" onclick={() => save(true, close)} disabled={!saveName.trim()}>Save as new</button>
                <button type="submit" class="btn sm primary" disabled={!saveName.trim()}>Update “{coll.name}”</button>
              {:else}
                <button type="submit" class="btn sm primary" disabled={!saveName.trim()}>Save</button>
              {/if}
            </div>
          </form>
        {/snippet}
      </Popover>
    {/if}

    <Popover align="right">
      {#snippet trigger(toggle, open)}
        <button class="btn sm" class:on={open} onclick={toggle} title="Sort"><Icon name="sort" />{store.mobile ? '' : sorts.find((s) => s[0] === f.sort)?.[1]}</button>
      {/snippet}
      {#snippet children(close)}
        {#each sorts as [k, label]}
          <button class="menu-item" class:active={f.sort === k} onclick={() => { store.setFilter('sort', k); close(); }}>
            {label}{#if f.sort === k}<Icon name="check" />{/if}
          </button>
        {/each}
        <div class="menu-sep"></div>
        <button class="menu-item" onclick={() => { store.setFilter('desc', !f.desc); close(); }}>
          {f.desc ? 'Descending' : 'Ascending'} <Icon name="sort" />
        </button>
      {/snippet}
    </Popover>

    {#if !store.mobile}
      <input class="size" type="range" min="110" max="420" step="10" value={store.settings.rowHeight} oninput={setRow} title="Thumbnail size" />
      <div class="seg">
        <button class:on={store.view === 'grid'} onclick={() => (store.view = 'grid')}><Icon name="grid" />Grid</button>
        <button class:on={store.view === 'map'} onclick={() => (store.view = 'map')}><Icon name="map" />Map</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .toolbar {
    min-height: var(--toolbar-h);
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.375rem 0.75rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
    flex-wrap: wrap;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 0.125rem;
    min-width: 0;
    flex: 1;
    overflow: hidden;
  }
  .crumbs :global(.sep) {
    color: var(--text-3);
    flex: none;
  }
  .crumb {
    border: 0;
    background: none;
    color: var(--text-2);
    font-weight: 600;
    cursor: pointer;
    padding: 0.25rem 0.375rem;
    border-radius: var(--r-sm);
    white-space: nowrap;
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  .crumb:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .crumb.current {
    color: var(--text);
    font-size: 1.0625rem;
    font-weight: 700;
  }
  .count {
    margin-left: 0.5rem;
    color: var(--text-3);
    font-size: var(--fs-sm);
    white-space: nowrap;
  }
  .crumbs .spinner {
    width: 0.875rem;
    height: 0.875rem;
    margin-left: 0.5rem;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .size {
    width: 6.5rem;
    accent-color: var(--accent-strong);
  }
  .filters {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.375rem 0.5rem 0.25rem;
  }
  .fl {
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-weight: 600;
    margin-top: 0.375rem;
  }
  .stars,
  .wrap {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .lbl {
    width: 1.75rem;
    height: 1.75rem;
    display: grid;
    place-items: center;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--surface);
    cursor: pointer;
  }
  .lbl.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent);
  }
  .seg.small > button {
    height: 1.75rem;
    font-size: var(--fs-sm);
    flex: 1;
  }
  .seg.small {
    display: flex;
  }
  .filters select {
    width: 100%;
    height: 2rem;
    font-size: var(--fs-sm);
  }
  .dates {
    display: flex;
    align-items: center;
    gap: 0.375rem;
  }
  .dates .field {
    flex: 1;
    min-width: 0;
    height: 2rem;
    font-size: var(--fs-sm);
    padding: 0 0.375rem;
  }
  .modified {
    font-size: var(--fs-sm);
    color: var(--critical);
    margin-left: 0.25rem;
    white-space: nowrap;
  }
  .savebox {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.375rem 0.5rem 0.25rem;
  }
  .savebox .fl {
    margin-top: 0;
  }
  .row.end {
    justify-content: flex-end;
    flex-wrap: wrap;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    margin-top: 0.375rem;
    border-top: 1px solid var(--border);
    padding-top: 0.375rem;
  }
  @media (max-width: 760px) {
    .crumb.current {
      display: none;
    }
    .count {
      margin-left: 0.25rem;
      font-size: var(--fs);
    }
    .toolbar {
      padding: 0.25rem 0.5rem;
      gap: 0.375rem;
      min-height: 2.75rem;
    }
    .crumb:not(.current) {
      display: none;
    }
    .crumbs :global(.sep) {
      display: none;
    }
  }
</style>
