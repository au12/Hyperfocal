<script lang="ts">
  import { store, LABELS, activeFilterCount, type SortKey } from '../lib/store.svelte';
  import { api, type Facets } from '../lib/api';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import { Tween } from 'svelte/motion';
  import { segmented, easeOut, reduced } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';

  let facets = $state<Facets | null>(null);
  let filterOpen = $state(false);
  let saveOpen = $state(false);
  let saveName = $state('');

  const crumbs = $derived(store.folderPath(store.folderId));
  const f = $derived(store.filters);
  const count = $derived(store.list.id.length);
  // The photo count rolls to its new value.
  const shown = new Tween(0, { duration: 420, easing: easeOut });
  $effect(() => {
    shown.set(count, { duration: reduced() ? 0 : 420 });
  });
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
    Object.assign(store.filters, { rmin: 0, unrated: false, lc: [], pick: null, kinds: [], gps: null, cam: '', lens: '', kw: '', onlyRejected: false, dateFrom: '', dateTo: '', subj: [], ids: [] });
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
    for (const id of f.subj ?? []) parts.push(store.subjLabels[id] ?? 'Person');
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

  const baseSorts: [SortKey, string][] = [
    ['taken', 'Capture time'],
    ['name', 'File name'],
    ['rating', 'Rating'],
    ['modified', 'File date'],
  ];
  const album = $derived(albums.current);
  const sorts = $derived<[SortKey, string][]>(albums.currentId != null ? [['album', 'Album order'], ...baseSorts] : baseSorts);

  // The album shown: rename in place, a note, delete.
  let editing = $state(false);
  let editText = $state('');
  let noteOpen = $state(false);
  let noteText = $state('');

  function startEdit(close: () => void) {
    close();
    if (!album) return;
    editText = album.name;
    editing = true;
  }

  function commitEdit() {
    editing = false;
    if (album) albums.rename(album, editText);
  }

  function deleteShown(close: () => void) {
    close();
    const a = album;
    if (a && confirm(`Delete the album “${a.name}”? Its photos stay where they are: only the album goes.`)) albums.deleteAlbum(a);
  }

  function setRow(e: Event) {
    store.settings.rowHeight = +(e.target as HTMLInputElement).value;
    store.persist();
  }
</script>

<div class="toolbar">
  <div class="crumbs">
    {#if coll}
      {#if coll.id === 'dupes'}
        <button class="crumb" onclick={() => (store.dialog = 'duplicates')} title="Back to the Duplicates report"><Icon name="copy" size={14} /> Duplicates</button>
        <Icon name="chevron" size={14} class="sep" />
        <span class="crumb current">{coll.name}</span>
      {:else if albums.currentId != null}
        {#if editing}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="field crumb-edit"
            bind:value={editText}
            maxlength="120"
            autofocus
            onkeydown={(e) => {
              if (e.key === 'Enter') commitEdit();
              else if (e.key === 'Escape') editing = false;
            }}
            onblur={commitEdit}
          />
        {:else}
          <button class="crumb current album-name" title={album?.note ?? 'Album: double-click to rename'} ondblclick={() => startEdit(() => {})}><Icon name="album" size={15} /> {coll.name}</button>
        {/if}
        <Popover align="left" width="16rem">
          {#snippet trigger(toggle, open)}
            <button class="btn sm ghost icon" class:on={open} onclick={toggle} aria-label="Album actions"><Icon name="more" /></button>
          {/snippet}
          {#snippet children(close)}
            <button class="menu-item" onclick={() => startEdit(close)}><Icon name="edit" />Rename</button>
            <button class="menu-item" onclick={() => { close(); noteText = album?.note ?? ''; noteOpen = true; }}><Icon name="tag" />{album?.note ? 'Edit the note' : 'Add a note'}</button>
            {#if store.list.id[store.cursor] != null}
              <button class="menu-item" onclick={() => { close(); albums.setCover(store.list.id[store.cursor]); }} title="The photo under the cursor becomes the album's picture"><Icon name="image" />Use the current photo as cover</button>
            {/if}
            {#if store.status?.immich && album}
              <button class="menu-item" onclick={() => { close(); if (album) albums.sendToImmich(album); }}><Icon name="send" />Send the album to Immich…</button>
            {/if}
            {#if album?.missing}
              <button class="menu-item" onclick={() => { close(); if (album) albums.forgetMissing(album); }}><Icon name="eye-off" />Forget {album.missing} missing</button>
            {/if}
            <div class="menu-sep"></div>
            <button class="menu-item danger" onclick={() => deleteShown(close)}><Icon name="trash" />Delete album</button>
          {/snippet}
        </Popover>
        {#if album?.missing}
          <span class="chip missing" title="Members whose file left the library (moved or renamed by another app). They come back by themselves if the file turns up.">{album.missing} missing</span>
        {/if}
      {:else}
        <span class="crumb current" title="Collection"><Icon name={typeof coll.id === 'number' ? 'bookmark' : 'library'} size={14} /> {coll.name}</span>
        {#if modified}<span class="modified" title="Filters or search differ from the saved collection">modified</span>{/if}
      {/if}
    {:else if store.query}
      {#if store.dateRange}<button class="crumb" onclick={() => store.search('')}>{store.dateRange.label}</button><Icon name="chevron" size={14} class="sep" />{/if}
      <span class="crumb current"><Icon name="search" size={14} /> “{store.query}”</span>
      <button class="btn sm ghost" onclick={() => store.search('')}>Clear</button>
    {:else if store.dateRange}
      <span class="crumb"><Icon name="calendar" size={14} /> Timeline</span>
      <Icon name="chevron" size={14} class="sep" />
      <span class="crumb current">{store.dateRange.label}</span>
    {:else if !(f.subj?.length && crumbs.length === 1)}
      {#each crumbs as c, i (c.id)}
        {#if i > 0}<Icon name="chevron" size={14} class="sep" />{/if}
        <button class="crumb" class:current={i === crumbs.length - 1} onclick={() => store.openFolder(c.id)}>
          {i === 0 ? 'Library' : c.name}
        </button>
      {/each}
    {/if}
    {#each f.subj ?? [] as sid (sid)}
      <span class="chip who" title="Photos with this person or pet"><Icon name="user" size={12} /><span class="ellipsis">{store.subjLabels[sid] ?? 'Person'}</span><button onclick={() => store.setFilter('subj', f.subj.filter((x) => x !== sid))} aria-label="Remove"><Icon name="x" size={12} /></button></span>
    {/each}
    <span class="count tnum">{Math.round(shown.current).toLocaleString()} {count === 1 ? 'photo' : 'photos'}</span>
    {#if albums.arranging && !store.mobile && count > 1}<span class="hint-arrange" title="Drag photos (or a selection) to put them in order"><Icon name="grip" size={13} /><span class="txt">Drag to arrange</span></span>{/if}
    {#if store.loading}<span class="spinner"></span>{/if}
  </div>

  {#if noteOpen && album}
    <form class="notebox" onsubmit={(e) => { e.preventDefault(); noteOpen = false; if (album) albums.setNote(album, noteText); }}>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="field" bind:value={noteText} placeholder="A few words about this album" maxlength="2000" autofocus onkeydown={(e) => e.key === 'Escape' && (noteOpen = false)} />
      <button class="btn sm primary" type="submit">Save</button>
      <button class="btn sm ghost" type="button" onclick={() => (noteOpen = false)}>Cancel</button>
    </form>
  {/if}

  <div class="tools">
    {#if !store.query && store.mobile && albums.currentId == null}
      <button class="btn sm icon" class:on={store.recursive} onclick={() => { store.recursive = !store.recursive; store.reload(); }} title="Include sub folders" aria-label="Include sub folders"><Icon name="layers" /></button>
    {/if}
    {#if !store.query && !store.mobile && albums.currentId == null}
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
          <div class="seg small" use:segmented>
            <button class:on={f.pick == null} onclick={() => store.setFilter('pick', null)}>All</button>
            <button class:on={f.pick === 1} onclick={() => store.setFilter('pick', 1)}>Picks</button>
            <button class:on={f.pick === 0} onclick={() => store.setFilter('pick', 0)}>Unflagged</button>
          </div>
          <div class="fl">File type</div>
          <div class="seg small" use:segmented>
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
          <div class="seg small" use:segmented>
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
          <div class="seg small" use:segmented>
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
      <div class="seg" use:segmented>
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
    padding: 0.4375rem 0.75rem 0.4375rem 1rem;
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
    container-type: inline-size;
  }
  /* Short of room, the arranging hint keeps only its icon (the tooltip says the rest). */
  @container (max-width: 30rem) {
    .hint-arrange .txt {
      display: none;
    }
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
    font-size: 1.125rem;
    font-weight: 680;
    letter-spacing: -0.025em;
    animation: crumb-in var(--dur-slow) var(--ease-out);
  }
  @keyframes crumb-in {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
  }
  .album-name {
    font: inherit;
  }
  .crumb-edit {
    height: 1.875rem;
    font-size: 1.0625rem;
    font-weight: 650;
    width: min(20rem, 50vw);
  }
  .chip.missing {
    margin-left: 0.25rem;
    color: var(--critical);
    background: color-mix(in srgb, var(--critical) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--critical) 35%, transparent);
  }
  .hint-arrange {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    margin-left: 0.625rem;
    font-size: var(--fs-xs);
    color: var(--text-3);
    white-space: nowrap;
    animation: crumb-in var(--dur-slow) var(--ease-out);
  }
  .notebox {
    flex-basis: 100%;
    order: 3;
    display: flex;
    gap: 0.5rem;
    align-items: center;
    animation: crumb-in var(--dur-slow) var(--ease-out);
  }
  .notebox .field {
    flex: 1;
  }
  .chip.who {
    margin-left: 0.25rem;
    flex: none;
    max-width: 14rem;
    font-weight: 600;
  }
  .count {
    margin-left: 0.625rem;
    color: var(--text-3);
    font-size: var(--fs-sm);
    font-weight: 500;
    white-space: nowrap;
    padding: 0.125rem 0.5rem;
    border-radius: 999px;
    background: var(--surface-2);
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
  /* Thumbnail size: a slim track with a round handle. */
  .size {
    width: 6.5rem;
    height: 1.25rem;
    appearance: none;
    -webkit-appearance: none;
    background: transparent;
    cursor: pointer;
  }
  .size::-webkit-slider-runnable-track {
    height: 3px;
    border-radius: 3px;
    background: var(--surface-3);
  }
  .size::-moz-range-track {
    height: 3px;
    border-radius: 3px;
    background: var(--surface-3);
  }
  .size::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 0.875rem;
    height: 0.875rem;
    margin-top: -0.3125rem;
    border-radius: 50%;
    background: #fff;
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.2),
      0 1px 4px rgba(0, 0, 0, 0.4);
    transition: transform var(--dur-fast) var(--ease-spring);
  }
  .size::-moz-range-thumb {
    width: 0.875rem;
    height: 0.875rem;
    border: 0;
    border-radius: 50%;
    background: #fff;
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.2),
      0 1px 4px rgba(0, 0, 0, 0.4);
  }
  .size:hover::-webkit-slider-thumb,
  .size:active::-webkit-slider-thumb {
    transform: scale(1.2);
  }
  .filters {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.375rem 0.5rem 0.25rem;
  }
  .fl {
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-weight: 650;
    margin-top: 0.5rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
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
    transition:
      transform var(--dur-fast) var(--ease-spring),
      box-shadow var(--dur);
  }
  .lbl:hover {
    transform: scale(1.08);
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
