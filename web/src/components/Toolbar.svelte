<script lang="ts">
  import { store, LABELS, LOCATIONS, activeFilterCount, type SortKey, type Filters } from '../lib/store.svelte';
    import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import FilterPanel from './FilterPanel.svelte';
  import { Tween } from 'svelte/motion';
  import { segmented, easeOut, reduced } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';
  import { cull, ISSUES } from '../lib/cull.svelte';
  import { mlPreparing, mlState } from '../lib/ml';

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

  function clearFilters() {
    Object.assign(store.filters, { rmin: 0, unrated: false, lc: [], pick: null, kinds: [], gps: onMap ? 1 : null, cam: '', lens: '', kw: '', onlyRejected: false, dateFrom: '', dateTo: '', subj: [], ids: [], ppl: '', shot: '', eyes: '', issue: [], weak: false });
    store.persist();
    store.reload();
  }

  /** The filters in effect, as chips under the toolbar: each one says what it does and
   * goes with a click, without opening the panel. */
  interface FChip {
    key: string;
    label: string;
    icon: string;
    tone?: string;
    clear: () => void;
  }
  const KIND_NAMES: Record<number, string> = { 1: 'RAW', 2: 'JPEG', 5: 'HEIC' };
  const fchips = $derived.by((): FChip[] => {
    const out: FChip[] = [];
    const set =
      <K extends keyof Filters>(k: K, v: Filters[K]) =>
      () =>
        store.setFilter(k, v);
    if (f.rmin > 0) out.push({ key: 'rmin', label: f.rmin === 5 ? '5★' : `${f.rmin}★ and up`, icon: 'star', clear: set('rmin', 0) });
    if (f.unrated) out.push({ key: 'unrated', label: 'Unrated', icon: 'star', clear: set('unrated', false) });
    if (f.pick === 1) out.push({ key: 'pick', label: 'Picks', icon: 'flag', clear: set('pick', null) });
    if (f.pick === 0) out.push({ key: 'pick', label: 'Unflagged', icon: 'flag', clear: set('pick', null) });
    for (const c of f.lc) {
      out.push({ key: 'lc-' + c, label: c === 'none' ? 'No label' : (LABELS.find((l) => l.color === c)?.name ?? c), icon: 'tag', tone: c === 'none' ? undefined : c, clear: () => store.setFilter('lc', f.lc.filter((x) => x !== c)) });
    }
    if (f.kinds.length) out.push({ key: 'kinds', label: f.kinds.length > 1 ? 'TIFF & PNG' : (KIND_NAMES[f.kinds[0]] ?? 'Other'), icon: 'image', clear: set('kinds', []) });
    if (f.dateFrom || f.dateTo) {
      const label = f.dateFrom && f.dateTo ? `${f.dateFrom} – ${f.dateTo}` : f.dateFrom ? `From ${f.dateFrom}` : `Until ${f.dateTo}`;
      out.push({ key: 'dates', label, icon: 'calendar', clear: () => { store.filters.dateFrom = ''; store.setFilter('dateTo', ''); } });
    }
    if (f.cam) out.push({ key: 'cam', label: f.cam, icon: 'camera', clear: set('cam', '') });
    if (f.lens) out.push({ key: 'lens', label: f.lens, icon: 'aperture', clear: set('lens', '') });
    if (f.gps === 1 && !onMap) out.push({ key: 'gps', label: 'With GPS', icon: 'pin', clear: set('gps', null) });
    if (f.gps === 0) out.push({ key: 'gps', label: 'Without GPS', icon: 'pin', clear: set('gps', null) });
    if (f.kw) out.push({ key: 'kw', label: f.kw.split('|').pop() ?? f.kw, icon: 'tag', clear: set('kw', '') });
    if (f.onlyRejected) out.push({ key: 'rej', label: 'Rejected only', icon: 'reject', clear: () => { store.filters.showRejected = false; store.setFilter('onlyRejected', false); } });
    if (f.ppl) out.push({ key: 'ppl', label: f.ppl === '0' ? 'No people' : `${f.ppl.replace('-', '–').replace(/–$/, '+')} ${f.ppl === '1' ? 'person' : 'people'}`, icon: 'users', clear: set('ppl', '') });
    if (f.shot) out.push({ key: 'shot', label: f.shot === 'portrait' ? 'Portraits' : 'Groups', icon: 'user', clear: set('shot', '') });
    if (f.eyes) out.push({ key: 'eyes', label: f.eyes === 'closed' ? 'Eyes closed' : 'Eyes open', icon: f.eyes === 'closed' ? 'eye-closed' : 'eye', clear: set('eyes', '') });
    for (const k of f.issue ?? []) {
      const is = ISSUES.find((x) => x.key === k);
      out.push({ key: 'issue-' + k, label: is?.label ?? k, icon: is?.icon ?? 'focus', tone: k, clear: () => store.setFilter('issue', (f.issue ?? []).filter((x) => x !== k)) });
    }
    if (f.weak) out.push({ key: 'weak', label: 'Weaker frames', icon: 'stack', clear: set('weak', false) });
    return out;
  });

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
  /** Reviews the series at the cursor, else the next one after it, else the first. */
  function reviewSeries() {
    const sk = store.list.sk;
    if (!sk) return;
    const c = store.cursor;
    let i = sk[c] ? c : -1;
    if (i < 0) for (let j = c + 1; j < sk.length; j++) if (sk[j]) { i = j; break; }
    if (i < 0) i = sk.findIndex((k) => !!k);
    if (i >= 0) cull.review(i);
  }
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
        <button class="btn sm cb-review" onclick={reviewSeries} title={`Go through the series one by one, frames side by side${store.keyHint('review')}`}><Icon name="layers" size={13} />Review {stacks} series</button>
        <button class="btn sm ghost" onclick={cull.expandAll} title="Show every frame of every series"><Icon name="stack" size={13} /><span class="cb-long">Unfold all</span></button>
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
          <span class="spinner"></span><span class="tnum">{cs.done.toLocaleString()}<span class="dim">/{cs.total.toLocaleString()}</span></span><span class="cb-long">measured</span>
        </span>
      {:else if cs?.phase === 'off'}
        <span class="cb-progress dim" title="HYPERFOCAL_CULL=0 is set">Measurements are off</span>
      {/if}
    </div>
  {/if}

  {#if fchips.length}
    <div class="fchips" aria-label="Filters in effect">
      {#each fchips as c (c.key)}
        <span class="fchip" data-tone={c.tone}>
          <Icon name={c.icon} size={12} />
          <span class="ellipsis">{c.label}</span>
          <button onclick={c.clear} aria-label={`Remove the filter: ${c.label}`} title="Remove"><Icon name="x" size={11} stroke={2.4} /></button>
        </span>
      {/each}
      {#if fchips.length > 1}<button class="clear-all" onclick={clearFilters}>Clear all</button>{/if}
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

    <Popover bind:open={filterOpen} align="right" width="25rem" flush sheet label="Filters">
      {#snippet trigger(toggle, open)}
        <button class="btn sm" class:on={open || activeFilters > 0} onclick={toggle} title="Filter the photos shown" aria-haspopup="dialog" aria-expanded={open}>
          <Icon name="filter" />{#if !store.mobile}<span class="t">Filter</span>{/if}
          {#if activeFilters > 0}<span class="badge">{activeFilters}</span>{/if}
        </button>
      {/snippet}
      {#snippet children(close)}
        <FilterPanel {onMap} {count} {close} />
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
    /* --edge-l / --edge-r: room for a hidden side panel's tab (App.svelte). */
    padding: 0.4375rem calc(0.75rem + var(--edge-r, 0px)) 0.4375rem calc(1rem + var(--edge-l, 0px));
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
    .cullbar .cb-label,
    .cullbar .cb-long {
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
    flex-wrap: nowrap;
    overflow-x: auto;
    scrollbar-width: none;
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
  .cullbar::-webkit-scrollbar {
    display: none;
  }
  .cullbar > * {
    flex: none;
  }
  .cullbar .grow {
    flex: 1 1 0;
    min-width: 0.5rem;
  }
  .cb-review {
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
    color: var(--text);
  }
  /* The filters in effect, one chip each. */
  .fchips {
    order: 11;
    flex-basis: 100%;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.3125rem;
    margin: -0.125rem 0 0.0625rem;
    animation: cullbar-in var(--dur-slow) var(--ease-out);
  }
  :global(.mobile) .fchips {
    flex-wrap: nowrap;
    overflow-x: auto;
    scrollbar-width: none;
    margin-right: -0.5rem;
    padding-right: 0.5rem;
  }
  :global(.mobile) .fchips > * {
    flex: none;
  }
  .fchip {
    display: inline-flex;
    align-items: center;
    gap: 0.3125rem;
    max-width: 16rem;
    height: 1.5rem;
    padding: 0 0.25rem 0 0.5rem;
    border-radius: 99px;
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--text);
    font-size: var(--fs-sm);
    font-weight: 560;
    animation: chip-in var(--dur) var(--ease-spring);
  }
  .fchip :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .fchip[data-tone='red'] :global(svg:first-child) { color: var(--label-red); }
  .fchip[data-tone='yellow'] :global(svg:first-child) { color: var(--label-yellow); }
  .fchip[data-tone='green'] :global(svg:first-child) { color: var(--label-green); }
  .fchip[data-tone='blue'] :global(svg:first-child) { color: var(--label-blue); }
  .fchip[data-tone='purple'] :global(svg:first-child) { color: var(--label-purple); }
  .fchip[data-tone='blur'] :global(svg:first-child) { color: #f59e0b; }
  .fchip[data-tone='eyes'] :global(svg:first-child) { color: #ff5c61; }
  .fchip[data-tone='over'] :global(svg:first-child) { color: #fb7a3c; }
  .fchip[data-tone='under'] :global(svg:first-child) { color: #6b8cff; }
  .fchip button {
    display: grid;
    place-items: center;
    width: 1.125rem;
    height: 1.125rem;
    border: 0;
    border-radius: 50%;
    background: none;
    color: var(--text-3);
    cursor: pointer;
    padding: 0;
  }
  .fchip button:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .fchip button :global(svg) {
    color: inherit;
  }
  .clear-all {
    border: 0;
    background: none;
    color: var(--text-3);
    font: inherit;
    font-size: var(--fs-sm);
    font-weight: 560;
    cursor: pointer;
    padding: 0 0.375rem;
    height: 1.5rem;
    border-radius: var(--r-sm);
  }
  .clear-all:hover {
    color: var(--text);
    background: var(--surface-2);
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
  .fl {
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-weight: 650;
    margin-top: 0.5rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
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
