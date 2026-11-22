<script lang="ts">
  import { store, LABELS, LOCATIONS, activeFilterCount, type SortKey } from '../lib/store.svelte';
  import { api, type Facets } from '../lib/api';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import { Tween } from 'svelte/motion';
  import { segmented, easeOut, reduced } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';
  import { cull, ISSUES } from '../lib/cull.svelte';
  import { mlPreparing, mlState } from '../lib/ml';

  let facets = $state<Facets | null>(null);
  let filterOpen = $state(false);
  let saveOpen = $state(false);
  let saveName = $state('');

  const crumbs = $derived(store.folderPath(store.folderId));
  const f = $derived(store.filters);
  /** Locations: the map takes only the filters (no folders, culling, order or sizes). */
  const onMap = $derived(store.view === 'map');
  /** Photos shown: a folded series counts all its frames. */
  const count = $derived.by(() => {
    void store.rev;
    const L = store.list;
    if (!L.sk || !L.sn) return L.id.length;
    let n = 0;
    for (let i = 0; i < L.id.length; i++) n += L.sk[i] && !cull.open.has(L.sk[i]) ? Math.max(1, L.sn[i]) : 1;
    return n;
  });
  // The photo count rolls to its new value.
  const shown = new Tween(0, { duration: 420, easing: easeOut });
  $effect(() => {
    shown.set(count, { duration: reduced() ? 0 : 420 });
  });
  // On the map, "with a location" is what Locations is, not a filter of yours.
  const activeFilters = $derived(activeFilterCount(f) - (onMap && f.gps === 1 ? 1 : 0));
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
    Object.assign(store.filters, { rmin: 0, unrated: false, lc: [], pick: null, kinds: [], gps: onMap ? 1 : null, cam: '', lens: '', kw: '', onlyRejected: false, dateFrom: '', dateTo: '', subj: [], ids: [], ppl: '', shot: '', eyes: '', issue: [], weak: false });
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
    if (f.ppl) parts.push(f.ppl === '0' ? 'No people' : `${f.ppl.replace('-', '–')} ${f.ppl === '1' ? 'person' : 'people'}`);
    if (f.shot) parts.push(f.shot === 'portrait' ? 'Portraits' : 'Groups');
    if (f.eyes) parts.push(f.eyes === 'closed' ? 'Eyes closed' : 'Eyes open');
    for (const k of f.issue ?? []) parts.push(ISSUES.find((x) => x.key === k)?.label ?? k);
    if (f.weak) parts.push('Weaker frames');
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
    ['size', 'File size'],
    ['focus', 'Sharpness'],
  ];

  function toggleIssue(k: string) {
    const cur = f.issue ?? [];
    store.setFilter('issue', cur.includes(k) ? cur.filter((x) => x !== k) : [...cur, k]);
  }

  const cs = $derived(store.status?.cull);
  /** Culling's measurements are still under way somewhere in the library. */
  const measuring = $derived(cull.on() && cs && cs.total > 0 && cs.done < cs.total && cs.phase !== 'off');
  const stacks = $derived.by(() => {
    void store.rev;
    const sk = store.list.sk;
    if (!sk) return 0;
    let n = 0;
    for (let i = 0; i < sk.length; i++) if (sk[i] && sk[i] !== sk[i - 1]) n++;
    return n;
  });
  const album = $derived(albums.current);
  const visual = $derived(store.visualShown());
  const sorts = $derived<[SortKey, string][]>([
    ...(visual ? ([['relevance', 'Relevance']] as [SortKey, string][]) : []),
    ...(albums.currentId != null ? ([['album', 'Album order']] as [SortKey, string][]) : []),
    ...baseSorts,
  ]);
  /** The sort in effect (a search by content is in order of relevance until another is chosen). */
  const sortNow = $derived<SortKey>(visual && store.relevance ? 'relevance' : f.sort);
  const vis = $derived(store.status?.visual);
  const looking = $derived(visual && vis && vis.total > 0 && vis.done < vis.total);
  // A search by content before its models are in: say so, with how far the download is.
  const mlWait = $derived(visual && mlPreparing(store.status?.ml) ? store.status?.ml : null);

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
      {#if coll.id === LOCATIONS.id}
        <span class="crumb current" title="Every photo with a location"><Icon name={LOCATIONS.icon} size={15} /> {LOCATIONS.name}</span>
      {:else if coll.id === 'dupes'}
        <button class="crumb" onclick={() => (store.dialog = 'duplicates')} title="Back to the Duplicates report"><Icon name="copy" size={14} /> Duplicates</button>
        <Icon name="chevron" size={14} class="sep" />
        <span class="crumb current">{coll.name}</span>
      {:else if coll.id === 'like'}
        <span class="crumb current smart-crumb" title="Photos that look like this one, most alike first"><Icon name="sparkles" size={15} /> {coll.name}</span>
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
      <span class="crumb current" class:smart-crumb={visual}><Icon name={visual ? 'sparkles' : 'search'} size={14} /> “{store.query}”</span>
      <button class="btn sm ghost" onclick={() => store.search('')}>Clear</button>
    {:else if store.dateRange}
      <span class="crumb"><Icon name="calendar" size={14} /> Timeline</span>
      <Icon name="chevron" size={14} class="sep" />
      <span class="crumb current">{store.dateRange.label}</span>
    {:else if !(f.subj?.length && crumbs.length === 1)}
      {#each crumbs as c, i (c.id)}
        {@const last = i === crumbs.length - 1}
        {#if i > 0}<Icon name="chevron" size={14} class="sep" />{/if}
        <button class="crumb" class:current={last} class:root={i === 0 && !last} onclick={() => store.openFolder(c.id)} title={i === 0 ? 'Library' : c.name}>
          {#if i === 0 && !last}<Icon name="library" size={15} class="root-icon" /><span class="root-label">Library</span>{:else}{i === 0 ? 'Library' : c.name}{/if}
        </button>
      {/each}
    {/if}
    {#each f.subj ?? [] as sid (sid)}
      <span class="chip who" title="Photos with this person or pet"><Icon name="user" size={12} /><span class="ellipsis">{store.subjLabels[sid] ?? 'Person'}</span><button onclick={() => store.setFilter('subj', f.subj.filter((x) => x !== sid))} aria-label="Remove"><Icon name="x" size={12} /></button></span>
    {/each}
    <span class="count tnum">{Math.round(shown.current).toLocaleString()} {count === 1 ? 'photo' : 'photos'}</span>
    {#if onMap && store.mapInView != null && store.mapInView < count}
      <span class="inview tnum" title="Photos within the part of the map on screen">{store.mapInView.toLocaleString()} in view</span>
    {/if}
    {#if mlWait}
      <span class="chip looking" title="Search by content needs its AI models, which are downloaded into the app data folder once"><span class="spinner"></span>{mlState(mlWait)}</span>
    {:else if looking && vis}
      <span class="chip looking" title="Search by content looks at every photo once, in the background (the newest first): results grow as it goes"><span class="spinner"></span>{vis.done.toLocaleString()} of {vis.total.toLocaleString()} looked at</span>
    {/if}
    {#if albums.arranging && !store.mobile && count > 1}<span class="hint-arrange" title="Drag photos (or a selection) to put them in order"><Icon name="grip" size={13} /><span class="txt">Drag to arrange</span></span>{/if}
    {#if store.loading}<span class="spinner"></span>{/if}
  </div>

  {#if cull.on()}
    <div class="cullbar">
      <span class="cb-title" title="Culling"><Icon name="focus" size={14} /><span class="t">Culling</span></span>
      {#if stacks > 0}
        <span class="cb-sep"></span>
        <button class="btn sm ghost" onclick={cull.expandAll} title="Show every frame of every series"><Icon name="stack" size={13} />Unfold {stacks} {stacks === 1 ? 'series' : 'series'}</button>
        {#if cull.open.size > 0}<button class="btn sm ghost" onclick={cull.collapseAll}>Fold all</button>{/if}
      {/if}
      <span class="cb-sep"></span>
      <span class="cb-label">Possible rejects</span>
      {#each ISSUES as is (is.key)}
        <button class="cb-chip {is.key}" class:on={(f.issue ?? []).includes(is.key)} onclick={() => toggleIssue(is.key)} title={is.hint}><Icon name={is.icon} size={12} />{is.label}</button>
      {/each}
      <button class="cb-chip weak" class:on={!!f.weak} onclick={() => store.setFilter('weak', !f.weak)} title="Every frame of a series but its best"><Icon name="stack" size={12} />Weaker frames</button>
      <span class="grow"></span>
      {#if measuring && cs}
        <span class="cb-progress" title="Every photo is measured once, in the background (the ones on screen first): sharpness of the subject from the RAW{cs.eyes ? ', closed eyes' : ''} and clipping">
          <span class="spinner"></span>{cs.done.toLocaleString()} of {cs.total.toLocaleString()} measured
        </span>
      {:else if cs?.phase === 'off'}
        <span class="cb-progress dim" title="HYPERFOCAL_CULL=0 is set">Measurements are off</span>
      {/if}
    </div>
  {/if}

  {#if noteOpen && album}
    <form class="notebox" onsubmit={(e) => { e.preventDefault(); noteOpen = false; if (album) albums.setNote(album, noteText); }}>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="field" bind:value={noteText} placeholder="A few words about this album" maxlength="2000" autofocus onkeydown={(e) => e.key === 'Escape' && (noteOpen = false)} />
      <button class="btn sm primary" type="submit">Save</button>
      <button class="btn sm ghost" type="button" onclick={() => (noteOpen = false)}>Cancel</button>
    </form>
  {/if}

  <div class="tools">
    {#if !store.query && store.mobile && albums.currentId == null && f.like == null && !onMap}
      <button class="btn sm icon" class:on={store.recursive} onclick={() => { store.recursive = !store.recursive; store.reload(); }} title="Include sub folders" aria-label="Include sub folders"><Icon name="layers" /></button>
    {/if}
    {#if !store.query && !store.mobile && albums.currentId == null && f.like == null && !onMap}
      <label class="check" title="Include photos in sub folders">
        <input type="checkbox" checked={store.recursive} onchange={(e) => { store.recursive = (e.target as HTMLInputElement).checked; store.reload(); }} />
        <span class="t lo">Sub folders</span><span class="t-short lo">Subs</span>
      </label>
    {/if}

    {#if !onMap}
      <button
        class="btn sm cull-toggle"
        class:on={cull.on()}
        onclick={cull.toggle}
        title={(cull.on() ? 'Culling on: series as stacks, possible rejects marked, faces side by side' : 'Culling: series as stacks, possible rejects marked, faces side by side') + store.keyHint('cull')}
        aria-pressed={cull.on()}
      >
        <Icon name="focus" />{#if !store.mobile}<span class="t">Cull</span>{/if}
      </button>
    {/if}

    <Popover bind:open={filterOpen} align="right" width="19rem">
      {#snippet trigger(toggle, open)}
        <button class="btn sm" class:on={open || activeFilters > 0} onclick={() => { toggle(); loadFacets(); }}>
          <Icon name="filter" />{#if !store.mobile}<span class="t">Filter</span>{/if}
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
          {#if !onMap}
            <div class="fl">Location</div>
            <div class="seg small" use:segmented>
              <button class:on={f.gps == null} onclick={() => store.setFilter('gps', null)}>All</button>
              <button class:on={f.gps === 1} onclick={() => store.setFilter('gps', 1)}>With GPS</button>
              <button class:on={f.gps === 0} onclick={() => store.setFilter('gps', 0)}>Without</button>
            </div>
          {/if}
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
          <div class="fl">People</div>
          <div class="seg small" use:segmented>
            {#each [['', 'Any'], ['0', 'None'], ['1', '1'], ['2', '2'], ['3-5', '3–5'], ['6-', '6+']] as [v, label] (v)}
              <button class:on={(f.ppl ?? '') === v} onclick={() => store.setFilter('ppl', v)}>{label}</button>
            {/each}
          </div>
          <div class="seg small" use:segmented>
            <button class:on={!f.shot} onclick={() => store.setFilter('shot', '')}>Any shot</button>
            <button class:on={f.shot === 'portrait'} onclick={() => store.setFilter('shot', 'portrait')} title="One person, close">Portraits</button>
            <button class:on={f.shot === 'group'} onclick={() => store.setFilter('shot', 'group')} title="Three people or more">Groups</button>
          </div>
          <div class="seg small" use:segmented>
            <button class:on={!f.eyes} onclick={() => store.setFilter('eyes', '')}>Any eyes</button>
            <button class:on={f.eyes === 'open'} onclick={() => store.setFilter('eyes', 'open')} title="Every face looked at has its eyes open">Eyes open</button>
            <button class:on={f.eyes === 'closed'} onclick={() => store.setFilter('eyes', 'closed')} title="Someone's eyes are closed">Eyes closed</button>
          </div>
          <div class="fl">Possible rejects <span class="dim hint">any of</span></div>
          <div class="row wrap">
            {#each ISSUES as is (is.key)}
              <button class="btn sm issue-chip {is.key}" class:on={(f.issue ?? []).includes(is.key)} onclick={() => toggleIssue(is.key)} title={is.hint}><Icon name={is.icon} size={13} />{is.label}</button>
            {/each}
            <button class="btn sm issue-chip weak" class:on={!!f.weak} onclick={() => store.setFilter('weak', !f.weak)} title="Every frame of a series but its best (the one you rated or picked, else open eyes and the sharpest)"><Icon name="stack" size={13} />Weaker frames</button>
          </div>
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

    {#if saveable && !onMap}
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

    {#if !onMap}
      <Popover align="right">
        {#snippet trigger(toggle, open)}
          <button class="btn sm" class:on={open} onclick={toggle} title={'Sort: ' + (sorts.find((s) => s[0] === sortNow)?.[1] ?? '')}><Icon name="sort" />{#if !store.mobile}<span class="t">{sorts.find((s) => s[0] === sortNow)?.[1]}</span>{/if}</button>
        {/snippet}
        {#snippet children(close)}
          {#each sorts as [k, label]}
            <button class="menu-item" class:active={sortNow === k} onclick={() => { store.setFilter('sort', k); close(); }}>
              {label}{#if sortNow === k}<Icon name="check" />{/if}
            </button>
          {/each}
          <div class="menu-sep"></div>
          <button class="menu-item" onclick={() => { store.setFilter('desc', !f.desc); close(); }}>
            {f.desc ? 'Descending' : 'Ascending'} <Icon name="sort" />
          </button>
        {/snippet}
      </Popover>
    {/if}

    {#if !store.mobile && !onMap}
      <input class="size" type="range" min="110" max="420" step="10" value={store.settings.rowHeight} oninput={setRow} title="Thumbnail size" />
    {/if}
  </div>
</div>

<style>
  .toolbar {
    container: toolbar / inline-size;
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
  /* Short of room, the tools give way before the folder's name: the thumbnail size
     narrows, then the buttons keep only their icons. */
  .t-short {
    display: none;
  }
  @container toolbar (max-width: 64rem) {
    .tools .size {
      width: 4.5rem;
    }
    /* The labels that say least go first, so the folder path keeps its room. */
    .tools .t.lo {
      display: none;
    }
    .tools .t-short.lo {
      display: inline;
    }
  }
  @container toolbar (max-width: 56rem) {
    .tools .t {
      display: none;
    }
    .tools .t-short {
      display: inline;
    }
    .tools .btn.sm {
      padding: 0 0.5rem;
    }
  }
  @container toolbar (max-width: 40rem) {
    .tools .size {
      display: none;
    }
  }
  @container toolbar (max-width: 60rem) {
    .cullbar .cb-title .t,
    .cullbar .cb-label {
      display: none;
    }
  }
  /* Culling on: the toggle glows, and a second row holds the stacks and the possible
     rejects. */
  .cull-toggle.on {
    background: linear-gradient(135deg, color-mix(in srgb, var(--accent-strong) 85%, #a35bff), var(--accent-strong));
    border-color: transparent;
    color: #fff;
    box-shadow: var(--shadow-glow);
  }
  .cull-toggle.on :global(svg) {
    animation: cull-spin 0.6s var(--ease-spring);
  }
  @keyframes cull-spin {
    from {
      transform: rotate(-90deg) scale(0.6);
    }
  }
  .cullbar {
    order: 10;
    flex-basis: 100%;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.375rem;
    margin: 0.125rem -0.25rem -0.125rem;
    padding: 0.375rem 0.25rem 0.0625rem;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
    animation: cullbar-in var(--dur-slow) var(--ease-out);
  }
  @keyframes cullbar-in {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  /* A phone: the chips scroll sideways in one row. */
  :global(.mobile) .cullbar {
    flex-wrap: nowrap;
    overflow-x: auto;
    scrollbar-width: none;
    margin-right: -0.75rem;
    padding-right: 0.75rem;
  }
  :global(.mobile) .cullbar::-webkit-scrollbar {
    display: none;
  }
  :global(.mobile) .cullbar > * {
    flex: none;
  }
  .cb-title {
    display: inline-flex;
    align-items: center;
    gap: 0.3125rem;
    font-weight: 680;
    color: var(--accent);
    letter-spacing: -0.01em;
  }
  .cb-sep {
    width: 1px;
    height: 1rem;
    background: var(--border-strong);
    margin: 0 0.25rem;
  }
  .cb-label {
    color: var(--text-3);
    font-weight: 550;
    margin-right: 0.125rem;
  }
  .cb-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3125rem;
    height: 1.625rem;
    padding: 0 0.625rem 0 0.5rem;
    border: 1px solid var(--border-strong);
    border-radius: 99px;
    background: var(--surface);
    color: var(--text-2);
    font: inherit;
    font-weight: 560;
    cursor: pointer;
    transition:
      background var(--dur-fast),
      color var(--dur-fast),
      border-color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out);
  }
  .cb-chip:hover {
    color: var(--text);
    border-color: color-mix(in srgb, var(--chip, var(--accent)) 50%, transparent);
  }
  .cb-chip:active {
    transform: scale(0.96);
  }
  .cb-chip :global(svg) {
    color: var(--chip, var(--accent));
  }
  .cb-chip.on {
    background: color-mix(in srgb, var(--chip, var(--accent)) 18%, transparent);
    border-color: color-mix(in srgb, var(--chip, var(--accent)) 60%, transparent);
    color: var(--text);
  }
  .cb-chip.blur,
  .issue-chip.blur {
    --chip: #f59e0b;
  }
  .cb-chip.eyes,
  .issue-chip.eyes {
    --chip: #ff5c61;
  }
  .cb-chip.over,
  .issue-chip.over {
    --chip: #fb7a3c;
  }
  .cb-chip.under,
  .issue-chip.under {
    --chip: #6b8cff;
  }
  .cb-chip.weak,
  .issue-chip.weak {
    --chip: var(--accent);
  }
  .issue-chip :global(svg) {
    color: var(--chip);
  }
  .cb-progress {
    display: inline-flex;
    align-items: center;
    gap: 0.4375rem;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .cullbar .grow {
    flex: 1;
  }
  .fl .hint {
    font-weight: 450;
    text-transform: none;
    letter-spacing: 0;
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
  /* Short of room, the folders above give way first (ellipsis), never the one shown. */
  button.crumb:not(.current) {
    display: block;
    flex-shrink: 1;
    min-width: 2.25rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .crumb.current {
    flex-shrink: 0;
    max-width: 100%;
  }
  /* The library at the start of a path: its name, or only its icon when room is short. */
  button.crumb.root {
    display: inline-flex;
    flex-shrink: 0;
    min-width: 0;
  }
  .crumb.root :global(.root-icon) {
    display: none;
  }
  @container (max-width: 34rem) {
    .crumb.root :global(.root-icon) {
      display: block;
    }
    .crumb.root .root-label {
      display: none;
    }
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
  .smart-crumb :global(svg) {
    color: #c287ff;
  }
  .chip.looking {
    margin-left: 0.5rem;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  .chip.looking .spinner {
    width: 0.75rem;
    height: 0.75rem;
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
  .inview {
    margin-left: 0.375rem;
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
