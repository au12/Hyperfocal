<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';
  import { store, QUICK, LOCATIONS, rangeLabel } from '../lib/store.svelte';
  import { urls, type Album, type Collection, type Folder } from '../lib/api';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import { people } from '../lib/people.svelte';
  import { albums } from '../lib/albums.svelte';
  import { flip } from 'svelte/animate';
  import { reduced, reveal } from '../lib/motion';
  import { contextMenu, type MenuBuilder, type MenuEntry } from '../lib/menu.svelte';
  import * as arrange from '../lib/arrange.svelte';
  import { DEFAULT_ORDER, SECTION_NAMES, type ArrangeGroup } from '../lib/arrange.svelte';

  let { onpick }: { onpick?: () => void } = $props();

  // Flatten the visible part of the tree (expanded folders only).
  const rows = $derived.by(() => {
    void store.treeRev;
    void store.expanded.size;
    const out: Folder[] = [];
    const walk = (f: Folder) => {
      out.push(f);
      if (store.expanded.has(f.id)) for (const c of f.children) walk(c);
    };
    for (const r of store.roots) {
      // The library root itself is the "All photos" entry; list its children.
      for (const c of r.children) walk(c);
    }
    return out;
  });

  const root = $derived(store.roots[0]);
  /** The grid is shown: what it shows is highlighted (not while the map or People is up). */
  const browsing = $derived(store.view === 'grid');
  const inFolders = $derived(browsing && !store.query && !store.dateRange && !store.collection);
  const peopleCount = $derived(people.status ? people.status.people + people.status.pets : undefined);

  function toggle(f: Folder, e: MouseEvent) {
    e.stopPropagation();
    if (store.expanded.has(f.id)) store.expanded.delete(f.id);
    else store.expanded.add(f.id);
  }

  // Every pick below shows its photos in the grid, leaving the map or People & pets.
  function open(f: Folder) {
    store.openFolder(f.id);
    onpick?.();
  }

  function openAll() {
    store.openLibrary();
    onpick?.();
  }

  function quick(q: (typeof QUICK)[number]) {
    store.applyCollection({ id: q.id, name: q.name, spec: q.spec });
    onpick?.();
  }

  function openSaved(c: Collection) {
    store.applyCollection({ id: c.id, name: c.name, spec: c.spec });
    onpick?.();
  }

  function openPeople() {
    store.openPeople();
    onpick?.();
  }

  function openLocations() {
    store.openLocations();
    onpick?.();
  }

  // ---- library tools: each opens its dialog
  let toolsShown = $state(localStorage.getItem('hf.tools') !== '0');

  function toggleTools() {
    toolsShown = !toolsShown;
    try {
      localStorage.setItem('hf.tools', toolsShown ? '1' : '0');
    } catch {
      /* ignore */
    }
  }

  function tool(d: 'keywords' | 'geotag' | 'duplicates' | 'integrity') {
    store.dialog = d;
    onpick?.();
  }

  const problems = $derived(store.integrity.changed + store.integrity.missing + store.integrity.unreadable);

  // ---- timeline: years > months > days, from per-day counts
  const DAY_MS = 86_400_000;
  interface TDay { day: number; n: number }
  interface TMonth { y: number; m: number; n: number; days: TDay[] }
  interface TYear { y: number; n: number; months: TMonth[] }
  const years = $derived.by((): TYear[] => {
    const t = store.timeline;
    if (!t) return [];
    const ys = new Map<number, TYear>();
    for (const [day, n] of t.days) {
      const d = new Date(day * DAY_MS);
      const y = d.getUTCFullYear();
      const m = d.getUTCMonth();
      let yr = ys.get(y);
      if (!yr) ys.set(y, (yr = { y, n: 0, months: [] }));
      let mo = yr.months.find((x) => x.m === m);
      if (!mo) yr.months.push((mo = { y, m, n: 0, days: [] }));
      yr.n += n;
      mo.n += n;
      mo.days.push({ day, n });
    }
    const out = [...ys.values()].sort((a, b) => b.y - a.y);
    for (const yr of out) {
      yr.months.sort((a, b) => a.m - b.m);
      for (const mo of yr.months) mo.days.sort((a, b) => a.day - b.day);
    }
    return out;
  });
  const tlOpen = new SvelteSet<string>();
  let timelineShown = $state(localStorage.getItem('hf.timeline') !== '0');

  function toggleTimeline() {
    timelineShown = !timelineShown;
    try {
      localStorage.setItem('hf.timeline', timelineShown ? '1' : '0');
    } catch {
      /* ignore */
    }
  }

  function tlToggle(key: string, e: MouseEvent) {
    e.stopPropagation();
    if (tlOpen.has(key)) tlOpen.delete(key);
    else tlOpen.add(key);
  }

  function openRange(from: number, to: number) {
    store.openDate({ from, to, label: rangeLabel(from, to) });
    onpick?.();
  }

  const isRange = (from: number, to: number) => browsing && !store.collection && store.dateRange != null && !store.dateRange.nodate && store.dateRange.from === from && store.dateRange.to === to;
  const yearRange = (y: number): [number, number] => [Date.UTC(y, 0, 1) / 1000, Date.UTC(y + 1, 0, 1) / 1000];
  const monthRange = (y: number, m: number): [number, number] => [Date.UTC(y, m, 1) / 1000, Date.UTC(y, m + 1, 1) / 1000];
  const monthName = (m: number) => new Date(Date.UTC(2000, m, 1)).toLocaleDateString(undefined, { month: 'long', timeZone: 'UTC' });
  const dayName = (day: number) => new Date(day * DAY_MS).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', timeZone: 'UTC' });

  // ---- saved collections: rename / update / delete
  let renaming = $state<number | null>(null);
  let renameText = $state('');

  function startRename(c: Collection, close: () => void) {
    close();
    renaming = c.id;
    renameText = c.name;
  }

  function commitRename(c: Collection) {
    const name = renameText.trim();
    renaming = null;
    if (name && name !== c.name) store.renameCollection(c, name);
  }

  function update(c: Collection, close: () => void) {
    close();
    store.saveCollection(c.name, c.id);
  }

  function remove(c: Collection, close: () => void) {
    close();
    if (confirm(`Delete the collection “${c.name}”? The photos stay where they are.`)) store.deleteCollection(c);
  }

  // ---- albums: create, open, rename, arrange (drag), delete
  let creating = $state(false);
  let newName = $state('');
  let renamingAlbum = $state<number | null>(null);
  let albumText = $state('');

  function startCreate() {
    creating = true;
    newName = '';
  }

  async function commitCreate() {
    const name = newName.trim();
    creating = false;
    if (!name) return;
    const a = await albums.create(name);
    if (a) openAlbum(a);
  }

  function openAlbum(a: Album) {
    albums.open(a);
    onpick?.();
  }

  function startAlbumRename(a: Album, close: () => void) {
    close();
    renamingAlbum = a.id;
    albumText = a.name;
  }

  function commitAlbumRename(a: Album) {
    renamingAlbum = null;
    albums.rename(a, albumText);
  }

  function deleteAlbum(a: Album, close: () => void) {
    close();
    if (confirm(`Delete the album “${a.name}”? Its ${a.count === 1 ? 'photo stays' : 'photos stay'} where ${a.count === 1 ? 'it is' : 'they are'}: only the album goes.`)) albums.deleteAlbum(a);
  }

  let dragAlbum = $state<number | null>(null);
  let dropAt = $state<{ id: number; after: boolean } | null>(null);

  function onAlbumDragStart(e: DragEvent, a: Album) {
    dragAlbum = a.id;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', a.name);
    }
  }

  function onAlbumDragOver(e: DragEvent, a: Album) {
    if (dragAlbum == null) return;
    e.preventDefault();
    if (dragAlbum === a.id) {
      dropAt = null;
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    dropAt = { id: a.id, after: e.clientY > r.top + r.height / 2 };
  }

  function onAlbumDrop(e: DragEvent) {
    e.preventDefault();
    const from = dragAlbum;
    const to = dropAt;
    dragAlbum = null;
    dropAt = null;
    if (from == null || !to) return;
    const ids = albums.list.map((a) => a.id).filter((id) => id !== from);
    const k = ids.indexOf(to.id) + (to.after ? 1 : 0);
    ids.splice(k, 0, from);
    albums.arrange(ids);
  }

  const fmt = (n: number | undefined) => (n == null ? '' : n >= 10000 ? `${(n / 1000).toFixed(0)}k` : n.toLocaleString());

  // ---- arrangement: the user's order of the sections and their entries, and what is
  // hidden (lib/arrange). A right-click, or a held touch, on any of them opens a menu to
  // move or hide it, which also lists what is hidden.
  interface Entry {
    id: string;
    name: string;
    icon: string;
    count?: number;
    active?: boolean;
    title?: string;
    alarm?: boolean;
    run: () => void;
  }

  const libEntries = $derived<Entry[]>([
    { id: 'all', name: 'All photos', icon: 'library', count: root?.total, active: inFolders && store.folderId === root?.id, run: openAll },
    ...(people.enabled ? [{ id: 'people', name: 'People & pets', icon: 'users', count: peopleCount, active: store.view === 'people', run: openPeople }] : []),
    { id: 'locations', name: LOCATIONS.name, icon: LOCATIONS.icon, count: store.counts[LOCATIONS.id], active: store.view === 'map', title: 'Every photo with a location, on a map' + store.keyHint('map'), run: openLocations },
    ...QUICK.map((q) => ({ id: q.id, name: q.name, icon: q.icon, count: store.counts[q.id], active: browsing && store.collection?.id === q.id, run: () => quick(q) })),
  ]);

  const toolEntries = $derived<Entry[]>([
    { id: 'keywords', name: 'Keywords', icon: 'tags', title: 'Rename, move, merge and remove keywords across the library', run: () => tool('keywords') },
    { id: 'geotag', name: 'Geotag from a track', icon: 'route', title: 'Place photos from a GPX, KML or location-history file', run: () => tool('geotag') },
    { id: 'duplicates', name: 'Duplicates', icon: 'copy', title: 'Exact copies, near-duplicates, look-alikes and series', run: () => tool('duplicates') },
    ...(store.status?.integrity !== false
      ? [{ id: 'integrity', name: 'Archive integrity', icon: problems ? 'shield-alert' : 'shield', title: 'Content hashes of the originals, checked over time', count: problems || undefined, alarm: problems > 0, run: () => tool('integrity') }]
      : []),
  ]);

  const sectionIds = $derived<string[]>(DEFAULT_ORDER.side.filter((id) => id !== 'collections' || store.collections.length > 0));
  const libIds = $derived(libEntries.map((e) => e.id));
  const toolIds = $derived(toolEntries.map((e) => e.id));
  const albumIds = $derived(albums.list.map((a) => String(a.id)));
  const collIds = $derived(store.collections.map((c) => String(c.id)));

  const pick = <T extends { id: string }>(all: T[], ids: string[]) => ids.map((id) => all.find((e) => e.id === id)).filter((e): e is T => !!e);
  const sections = $derived(arrange.visible('side', sectionIds));
  const libShown = $derived(pick(libEntries, arrange.visible('side.library', libIds)));
  const toolsList = $derived(pick(toolEntries, arrange.visible('side.tools', toolIds)));
  const albumsShown = $derived(albums.list.filter((a) => !arrange.isHidden('side.albums', String(a.id))));
  const collsShown = $derived(
    arrange
      .visible('side.collections', collIds)
      .map((id) => store.collections.find((c) => String(c.id) === id))
      .filter((c): c is Collection => !!c),
  );

  const GROUPS: ArrangeGroup[] = ['side', 'side.library', 'side.tools', 'side.albums', 'side.collections'];

  const item = (text: string, icon: string, run: () => void, more: Partial<Extract<MenuEntry, { kind: 'item' }>> = {}): MenuEntry => ({ kind: 'item', text, icon, run, ...more });

  /** Move up / Move down (the menu stays, so it can be pressed again) and Hide. */
  function arrangeEntries(g: ArrangeGroup, ids: string[], id: string, what: string): MenuEntry[] {
    return [
      item(`Move ${what}up`, 'arrow-up', () => arrange.move(g, ids, id, -1), { disabled: !arrange.canMove(g, ids, id, -1), stay: true }),
      item(`Move ${what}down`, 'arrow-down', () => arrange.move(g, ids, id, 1), { disabled: !arrange.canMove(g, ids, id, 1), stay: true }),
      item(`Hide ${what || 'from the sidebar'}`.trim(), 'eye-off', () => arrange.hide(g, id)),
    ];
  }

  /** What is hidden, each to show again, and the way back to the defaults. */
  function hiddenEntries(): MenuEntry[] {
    const out: MenuEntry[] = [];
    const add = (g: ArrangeGroup, id: string, name: string, hint: string) => out.push(item(name, 'eye', () => arrange.show(g, id), { hint, stay: true }));
    for (const id of arrange.hiddenOf('side', sectionIds)) add('side', id, SECTION_NAMES[id] ?? id, 'Section');
    for (const id of arrange.hiddenOf('side.library', libIds)) add('side.library', id, libEntries.find((e) => e.id === id)?.name ?? id, 'Library');
    for (const id of arrange.hiddenOf('side.tools', toolIds)) add('side.tools', id, toolEntries.find((e) => e.id === id)?.name ?? id, 'Tool');
    for (const a of albums.list) if (arrange.isHidden('side.albums', String(a.id))) add('side.albums', String(a.id), a.name, 'Album');
    for (const id of arrange.hiddenOf('side.collections', collIds)) add('side.collections', id, store.collections.find((c) => String(c.id) === id)?.name ?? id, 'Collection');
    const tail: MenuEntry[] = [];
    if (out.length > 1) tail.push(item('Show all', 'eye', showAll));
    if (customized()) tail.push(item('Reset the sidebar', 'restore', () => arrange.reset(...GROUPS), { hint: 'Order and hidden' }));
    const MAX = 12;
    const more: MenuEntry[] = out.length > MAX ? [{ kind: 'label', text: `…and ${out.length - MAX} more` }] : [];
    return [
      ...(out.length ? [{ kind: 'sep' } as MenuEntry, { kind: 'label', text: `Hidden (${out.length})` } as MenuEntry, ...out.slice(0, MAX), ...more] : []),
      ...(tail.length ? [{ kind: 'sep' } as MenuEntry, ...tail] : []),
    ];
  }

  function showAll() {
    for (const g of GROUPS) {
      const a = store.settings.arrange?.[g];
      if (a?.hidden?.length) store.settings.arrange = { ...store.settings.arrange, [g]: { ...a, hidden: [] } };
    }
    store.persist();
  }

  const customized = () => GROUPS.some((g) => store.settings.arrange?.[g]?.order?.length || store.settings.arrange?.[g]?.hidden?.length);

  function sectionMenu(id: string): MenuEntry[] {
    return [{ kind: 'label', text: `${SECTION_NAMES[id] ?? id} section` }, ...arrangeEntries('side', sectionIds, id, 'section '), ...hiddenEntries()];
  }

  function entryMenu(g: 'side.library' | 'side.tools', id: string): MenuEntry[] {
    const all = g === 'side.library' ? libEntries : toolEntries;
    const e = all.find((x) => x.id === id);
    if (!e) return sidebarMenu();
    return [{ kind: 'label', text: e.name }, item('Open', g === 'side.tools' ? 'external-link' : 'image', e.run), { kind: 'sep' }, ...arrangeEntries(g, all.map((x) => x.id), id, ''), ...hiddenEntries()];
  }

  /** Albums keep the library's own order (shared with every device): moving one saves it. */
  function moveAlbum(id: number, d: -1 | 1) {
    const next = arrange.movedOrder(albumIds, (x) => arrange.isHidden('side.albums', x), String(id), d);
    if (next) albums.arrange(next.map(Number));
  }
  const canMoveAlbum = (id: number, d: -1 | 1) => arrange.movedOrder(albumIds, (x) => arrange.isHidden('side.albums', x), String(id), d) != null;

  function albumMenu(id: number): MenuEntry[] {
    const a = albums.byId(id);
    if (!a) return sidebarMenu();
    const none = () => {};
    return [
      { kind: 'label', text: a.name },
      item('Open', 'album', () => openAlbum(a)),
      item('Rename', 'edit', () => startAlbumRename(a, none)),
      item(albums.target === a.id ? 'Stop adding here with B' : 'Add here with B', 'album-plus', () => albums.setTarget(albums.target === a.id ? null : a.id)),
      ...(store.status?.immich ? [item('Send to Immich…', 'send', () => albums.sendToImmich(a))] : []),
      ...(a.missing ? [item(`Forget ${a.missing} missing`, 'eye-off', () => albums.forgetMissing(a))] : []),
      { kind: 'sep' },
      item('Move up', 'arrow-up', () => moveAlbum(a.id, -1), { disabled: !canMoveAlbum(a.id, -1), stay: true }),
      item('Move down', 'arrow-down', () => moveAlbum(a.id, 1), { disabled: !canMoveAlbum(a.id, 1), stay: true }),
      item('Hide from the sidebar', 'eye-off', () => arrange.hide('side.albums', String(a.id))),
      { kind: 'sep' },
      item('Delete album', 'trash', () => deleteAlbum(a, none), { danger: true }),
      ...hiddenEntries(),
    ];
  }

  function collectionMenu(id: number): MenuEntry[] {
    const c = store.collections.find((x) => x.id === id);
    if (!c) return sidebarMenu();
    const none = () => {};
    return [
      { kind: 'label', text: c.name },
      item('Open', 'bookmark', () => openSaved(c)),
      item('Rename', 'edit', () => startRename(c, none)),
      item('Update to this view', 'refresh', () => update(c, none), { disabled: store.collection?.id !== c.id }),
      { kind: 'sep' },
      ...arrangeEntries('side.collections', collIds, String(c.id), ''),
      { kind: 'sep' },
      item('Delete', 'x', () => remove(c, none), { danger: true }),
      ...hiddenEntries(),
    ];
  }

  /** Right-click on the sidebar itself, between its entries. */
  function sidebarMenu(): MenuEntry[] {
    const rest = hiddenEntries();
    return [
      { kind: 'label', text: 'Sidebar' },
      ...(rest.length ? rest.slice(1) : [item('Nothing is hidden', 'eye', () => {}, { disabled: true })]),
      ...(rest.length ? [] : [{ kind: 'label', text: 'Right-click a section or an entry to move or hide it' } as MenuEntry]),
    ];
  }

  function buildMenu(el: HTMLElement): MenuBuilder | null {
    const key = el.dataset.el ?? '';
    const i = key.indexOf(':');
    const kind = i < 0 ? '' : key.slice(0, i);
    const id = i < 0 ? '' : key.slice(i + 1);
    switch (kind) {
      case 'sec':
        return () => sectionMenu(id);
      case 'lib':
        return () => entryMenu('side.library', id);
      case 'tool':
        return () => entryMenu('side.tools', id);
      case 'album':
        return () => albumMenu(+id);
      case 'coll':
        return () => collectionMenu(+id);
      default:
        return sidebarMenu;
    }
  }
</script>

<nav class="sidebar" use:contextMenu={{ build: buildMenu }}>
  {#each sections as sid (sid)}
    <div class="section {sid}" data-el="sec:{sid}" animate:flip={{ duration: reduced() ? 0 : 260 }}>
      {#if sid === 'library'}
        {@render librarySection()}
      {:else if sid === 'albums'}
        {@render albumsSection()}
      {:else if sid === 'collections'}
        {@render collectionsSection()}
      {:else if sid === 'tools'}
        {@render toolsSection()}
      {:else if sid === 'timeline'}
        {@render timelineSection()}
      {:else if sid === 'folders'}
        {@render foldersSection()}
      {/if}
    </div>
  {/each}
</nav>

{#snippet entryContent(e: Entry)}
  <Icon name={e.icon} size={16} /><span class="grow ellipsis">{e.name}</span>
  {#if e.count != null}<span class="count" class:bad={e.alarm} class:tnum={e.alarm}>{fmt(e.count)}</span>{/if}
{/snippet}

{#snippet librarySection()}
  <div class="sec-title">Library</div>
  {#each libShown as e (e.id)}
    <button class="item" class:active={e.active} data-el="lib:{e.id}" onclick={e.run} title={e.title} animate:flip={{ duration: reduced() ? 0 : 260 }}>
      {@render entryContent(e)}
    </button>
  {/each}
{/snippet}

{#snippet albumsSection()}
  <div class="sec-head">
    <div class="sec-title">Albums</div>
    <button class="sec-add" onclick={startCreate} title="New album" aria-label="New album"><Icon name="plus" size={14} stroke={2.25} /></button>
  </div>
  {#if creating}
    <div class="item coll" transition:reveal>
      <Icon name="album" size={16} />
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="rename"
        bind:value={newName}
        placeholder="Album name"
        maxlength="120"
        autofocus
        onkeydown={(e) => {
          if (e.key === 'Enter') commitCreate();
          else if (e.key === 'Escape') creating = false;
        }}
        onblur={commitCreate}
      />
    </div>
  {/if}
  {#each albumsShown as a (a.id)}
    <div
      class="item coll album"
      class:active={browsing && albums.currentId === a.id}
      class:dragging={dragAlbum === a.id}
      class:drop-before={dropAt?.id === a.id && !dropAt.after}
      class:drop-after={dropAt?.id === a.id && dropAt.after}
      data-el="album:{a.id}"
      draggable={renamingAlbum !== a.id && !store.mobile}
      ondragstart={(e) => onAlbumDragStart(e, a)}
      ondragover={(e) => onAlbumDragOver(e, a)}
      ondrop={onAlbumDrop}
      ondragend={() => { dragAlbum = null; dropAt = null; }}
      animate:flip={{ duration: reduced() ? 0 : 260 }}
      role="listitem"
    >
      {#if renamingAlbum === a.id}
        <span class="cover"><Icon name="album" size={13} /></span>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="rename"
          bind:value={albumText}
          maxlength="120"
          autofocus
          onkeydown={(e) => {
            if (e.key === 'Enter') commitAlbumRename(a);
            else if (e.key === 'Escape') renamingAlbum = null;
          }}
          onblur={() => commitAlbumRename(a)}
        />
      {:else}
        <button class="main" onclick={() => openAlbum(a)} title={a.note ? `${a.name}\n${a.note}` : a.name}>
          <span class="cover">
            {#if a.cover}<img src={urls.thumb(a.cover.id, a.cover.v)} alt="" loading="lazy" draggable="false" />{:else}<Icon name="album" size={13} />{/if}
          </span>
          <span class="grow ellipsis">{a.name}</span>
          {#if albums.target === a.id}<span class="target" title={'Photos go here with ' + (store.keyHint('albumAdd').replace(/[()\s]/g, '') || 'B')}>{store.keyHint('albumAdd').replace(/[()\s]/g, '') || 'B'}</span>{/if}
          <span class="count">{fmt(a.count)}</span>
        </button>
        <Popover align="right" width="15rem">
          {#snippet trigger(toggle)}
            <button class="more" onclick={toggle} aria-label={`Actions for ${a.name}`}><Icon name="more" size={16} /></button>
          {/snippet}
          {#snippet children(close)}
            <button class="menu-item" onclick={() => startAlbumRename(a, close)}><Icon name="edit" />Rename</button>
            <button class="menu-item" onclick={() => { close(); albums.setTarget(albums.target === a.id ? null : a.id); }}>
              <Icon name="album-plus" />{albums.target === a.id ? 'Stop adding here with B' : 'Add here with B'}
            </button>
            {#if store.status?.immich}
              <button class="menu-item" onclick={() => { close(); albums.sendToImmich(a); }}><Icon name="send" />Send to Immich…</button>
            {/if}
            {#if a.missing}
              <button class="menu-item" onclick={() => { close(); albums.forgetMissing(a); }} title="Members whose file left the library (moved or renamed by another app). They come back by themselves if the file turns up.">
                <Icon name="eye-off" />Forget {a.missing} missing
              </button>
            {/if}
            <button class="menu-item" onclick={() => { close(); arrange.hide('side.albums', String(a.id)); }}><Icon name="eye-off" />Hide from the sidebar</button>
            <div class="menu-sep"></div>
            <button class="menu-item danger" onclick={() => deleteAlbum(a, close)}><Icon name="trash" />Delete album</button>
          {/snippet}
        </Popover>
      {/if}
    </div>
  {/each}
  {#if albums.loaded && albums.list.length === 0 && !creating}
    <button class="item ghost-item" onclick={startCreate}><Icon name="album-plus" size={16} /><span class="grow">New album…</span></button>
  {/if}
{/snippet}

{#snippet collectionsSection()}
  <div class="sec-title">Collections</div>
  {#each collsShown as c (c.id)}
    <div class="item coll" class:active={browsing && store.collection?.id === c.id} data-el="coll:{c.id}" animate:flip={{ duration: reduced() ? 0 : 260 }}>
      {#if renaming === c.id}
        <Icon name="bookmark" size={16} />
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="rename"
          bind:value={renameText}
          autofocus
          onkeydown={(e) => {
            if (e.key === 'Enter') commitRename(c);
            else if (e.key === 'Escape') renaming = null;
          }}
          onblur={() => commitRename(c)}
        />
      {:else}
        <button class="main" onclick={() => openSaved(c)} title={c.name}>
          <Icon name="bookmark" size={16} /><span class="grow ellipsis">{c.name}</span><span class="count">{fmt(store.counts[String(c.id)])}</span>
        </button>
        <Popover align="right" width="13rem">
          {#snippet trigger(toggle)}
            <button class="more" onclick={toggle} aria-label={`Actions for ${c.name}`}><Icon name="more" size={16} /></button>
          {/snippet}
          {#snippet children(close)}
            <button class="menu-item" onclick={() => startRename(c, close)}><Icon name="edit" />Rename</button>
            <button class="menu-item" onclick={() => update(c, close)} disabled={store.collection?.id !== c.id} title="Save the filters and search on screen into this collection"><Icon name="refresh" />Update to this view</button>
            <button class="menu-item" onclick={() => { close(); arrange.hide('side.collections', String(c.id)); }}><Icon name="eye-off" />Hide from the sidebar</button>
            <div class="menu-sep"></div>
            <button class="menu-item danger" onclick={() => remove(c, close)}><Icon name="x" />Delete</button>
          {/snippet}
        </Popover>
      {/if}
    </div>
  {/each}
{/snippet}

{#snippet toolsSection()}
  <button class="sec-title toggle" class:open={toolsShown} onclick={toggleTools} aria-expanded={toolsShown}>
    <Icon name="chevron" size={12} stroke={2.25} />Library tools
  </button>
  {#if toolsShown}
    {#each toolsList as e (e.id)}
      <button class="item" class:alarm={e.alarm} data-el="tool:{e.id}" onclick={e.run} title={e.title} animate:flip={{ duration: reduced() ? 0 : 260 }}>
        {@render entryContent(e)}
      </button>
    {/each}
  {/if}
{/snippet}

{#snippet timelineSection()}
  <button class="sec-title toggle" class:open={timelineShown} onclick={toggleTimeline} aria-expanded={timelineShown}>
    <Icon name="chevron" size={12} stroke={2.25} />Timeline
  </button>
  {#if timelineShown}
    {#each years as yr (yr.y)}
      {@const [yf, yt] = yearRange(yr.y)}
      <div class="item" class:active={isRange(yf, yt)} onclick={() => openRange(yf, yt)} onkeydown={(e) => e.key === 'Enter' && openRange(yf, yt)} role="treeitem" aria-selected={isRange(yf, yt)} aria-expanded={tlOpen.has(`y${yr.y}`)} tabindex="0">
        <button class="tw" class:open={tlOpen.has(`y${yr.y}`)} onclick={(e) => tlToggle(`y${yr.y}`, e)} aria-label="Expand"><Icon name="chevron" size={14} /></button>
        <Icon name="calendar" size={16} />
        <span class="grow">{yr.y}</span><span class="count">{fmt(yr.n)}</span>
      </div>
      {#if tlOpen.has(`y${yr.y}`)}
        {#each yr.months as mo (mo.m)}
          {@const [mf, mt] = monthRange(mo.y, mo.m)}
          {@const mk = `m${mo.y}-${mo.m}`}
          <div class="item" style:padding-left="1.5rem" class:active={isRange(mf, mt)} onclick={() => openRange(mf, mt)} onkeydown={(e) => e.key === 'Enter' && openRange(mf, mt)} role="treeitem" aria-selected={isRange(mf, mt)} aria-expanded={tlOpen.has(mk)} tabindex="0">
            <button class="tw" class:open={tlOpen.has(mk)} onclick={(e) => tlToggle(mk, e)} aria-label="Expand"><Icon name="chevron" size={14} /></button>
            <span class="grow">{monthName(mo.m)}</span><span class="count">{fmt(mo.n)}</span>
          </div>
          {#if tlOpen.has(mk)}
            {#each mo.days as d (d.day)}
              <button class="item" style:padding-left="3.625rem" class:active={isRange(d.day * 86400, (d.day + 1) * 86400)} onclick={() => openRange(d.day * 86400, (d.day + 1) * 86400)}>
                <span class="grow">{dayName(d.day)}</span><span class="count">{fmt(d.n)}</span>
              </button>
            {/each}
          {/if}
        {/each}
      {/if}
    {/each}
    {#if store.timeline?.undated}
      <button class="item" class:active={browsing && !store.collection && store.dateRange?.nodate} onclick={() => { store.openDate({ from: 0, to: 0, label: 'No date', nodate: true }); onpick?.(); }}>
        <span class="tw"></span><Icon name="calendar" size={16} /><span class="grow dim">No date</span><span class="count">{fmt(store.timeline.undated)}</span>
      </button>
    {/if}
    {#if store.timeline && years.length === 0 && !store.timeline.undated}
      <div class="empty dim">No dated photos yet.</div>
    {/if}
  {/if}
{/snippet}

{#snippet foldersSection()}
  <div class="sec-title">Folders</div>
  {#each rows as f (f.id)}
    <div
      class="item"
      class:active={inFolders && store.folderId === f.id}
      class:reject={f.isReject}
      style:padding-left={`${0.5 + (f.depth - 1) * 1}rem`}
      onclick={() => open(f)}
      onkeydown={(e) => e.key === 'Enter' && open(f)}
      role="treeitem"
      aria-selected={store.folderId === f.id}
      aria-expanded={f.children.length ? store.expanded.has(f.id) : undefined}
      tabindex="0"
    >
      {#if f.children.length}
        <button class="tw" class:open={store.expanded.has(f.id)} onclick={(e) => toggle(f, e)} aria-label="Expand">
          <Icon name="chevron" size={14} />
        </button>
      {:else}
        <span class="tw"></span>
      {/if}
      <Icon name={f.isReject ? 'folder-x' : 'folder'} size={16} />
      <span class="grow ellipsis" title={f.path}>{f.name}</span>
      <span class="count">{fmt(f.isReject ? f.count : f.total)}</span>
    </div>
  {/each}
  {#if rows.length === 0}
    <div class="empty dim">No folders yet.</div>
  {/if}
{/snippet}

<style>
  .sidebar {
    width: var(--sidebar-w);
    flex: none;
    /* A held touch opens the sidebar's own menu, not the browser's callout. */
    -webkit-touch-callout: none;
    border-right: 1px solid var(--border);
    background: var(--surface);
    overflow-y: auto;
    overflow-x: hidden;
    padding: 0.625rem 0.5rem 2rem;
  }
  :global(.drawer) .sidebar {
    width: 100%;
    border: 0;
  }
  .section + .section {
    margin-top: 0.875rem;
  }
  .sec-title {
    font-size: var(--fs-xs);
    font-weight: 650;
    color: var(--text-3);
    padding: 0.25rem 0.625rem 0.375rem;
    text-transform: uppercase;
    letter-spacing: 0.075em;
  }
  .sec-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-right: 0.25rem;
  }
  .sec-add {
    width: 1.375rem;
    height: 1.375rem;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-3);
    display: grid;
    place-items: center;
    cursor: pointer;
    transition:
      background-color var(--dur-fast),
      color var(--dur-fast),
      transform var(--dur) var(--ease-spring);
  }
  .sec-add:hover {
    background: var(--surface-3);
    color: var(--accent);
    transform: rotate(90deg);
  }
  .album .cover {
    width: 1.25rem;
    height: 1.25rem;
    flex: none;
    border-radius: 0.3125rem;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .album .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    transition: transform var(--dur-slow) var(--ease-out);
  }
  .album:hover .cover img {
    transform: scale(1.12);
  }
  .album.active .cover {
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .target {
    font-size: 0.5625rem;
    font-weight: 750;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: 0.25rem;
    padding: 0 0.25rem;
    line-height: 0.9375rem;
    animation: chip-in var(--dur) var(--ease-spring);
  }
  .album.dragging {
    opacity: 0.4;
  }
  .album.drop-before {
    box-shadow: inset 0 2px 0 var(--accent);
  }
  .album.drop-after {
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .ghost-item {
    color: var(--text-3);
  }
  .ghost-item:hover {
    color: var(--accent);
  }
  .sec-title.toggle {
    display: flex;
    align-items: center;
    gap: 0.3125rem;
    width: 100%;
    border: 0;
    background: none;
    cursor: pointer;
    text-align: left;
    padding-left: 0.5rem;
    transition: color var(--dur-fast);
  }
  .sec-title.toggle:hover {
    color: var(--text-2);
  }
  .sec-title.toggle :global(svg) {
    transition: transform var(--dur) var(--ease-out);
  }
  .sec-title.toggle.open :global(svg) {
    transform: rotate(90deg);
  }
  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.5625rem;
    width: 100%;
    min-height: 1.875rem;
    padding: 0 0.5rem;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text-2);
    cursor: pointer;
    text-align: left;
    user-select: none;
    font-weight: 500;
    transition:
      background-color var(--dur-fast),
      color var(--dur-fast);
  }
  .item :global(svg) {
    color: var(--text-3);
    flex: none;
    transition: color var(--dur-fast);
  }
  .item:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .item:hover :global(svg) {
    color: var(--text-2);
  }
  .item.active {
    background: var(--accent-bg);
    color: var(--text);
    font-weight: 600;
  }
  .item.active :global(svg) {
    color: var(--accent);
  }
  /* The active item's marker: a short bar that grows in at the left edge. */
  .item::before {
    content: '';
    position: absolute;
    left: 0;
    top: 50%;
    width: 3px;
    height: 0;
    border-radius: 0 3px 3px 0;
    background: var(--accent);
    transform: translateY(-50%);
    transition: height var(--dur-slow) var(--ease-spring);
  }
  .item.active::before {
    height: 1rem;
  }
  .item.reject {
    color: var(--text-3);
    font-style: italic;
  }
  .item.reject :global(svg) {
    color: var(--reject);
  }
  .coll {
    padding: 0 0.125rem 0 0.5rem;
    gap: 0.125rem;
  }
  .coll .main {
    display: flex;
    align-items: center;
    gap: 0.5625rem;
    flex: 1;
    min-width: 0;
    height: 1.875rem;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
    padding: 0;
    text-align: left;
  }
  .more {
    width: 1.5rem;
    height: 1.5rem;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-3);
    display: grid;
    place-items: center;
    cursor: pointer;
    opacity: 0;
    transition:
      opacity var(--dur-fast),
      background-color var(--dur-fast);
  }
  .coll:hover .more,
  .coll.active .more,
  .more:focus-visible {
    opacity: 1;
  }
  .more:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .rename {
    flex: 1;
    min-width: 0;
    height: 1.625rem;
    border: 1px solid var(--accent);
    border-radius: var(--r-sm);
    background: var(--field);
    padding: 0 0.375rem;
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-bg);
  }
  .menu-item.danger,
  .menu-item.danger :global(svg) {
    color: var(--negative);
  }
  .menu-item:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .tw {
    width: 1.125rem;
    height: 1.125rem;
    display: grid;
    place-items: center;
    flex: none;
    border: 0;
    background: none;
    padding: 0;
    color: var(--text-3);
    cursor: pointer;
    border-radius: var(--r-sm);
    transition: background-color var(--dur-fast);
  }
  .tw :global(svg) {
    transition: transform var(--dur) var(--ease-out);
  }
  .tw.open :global(svg) {
    transform: rotate(90deg);
  }
  .tw:hover {
    background: var(--surface-3);
  }
  .count {
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
    font-weight: 500;
  }
  .item.active .count {
    color: var(--accent);
  }
  /* Archive integrity with problems: the shield and the count turn red. */
  .item.alarm :global(svg),
  .count.bad {
    color: var(--negative);
  }
  .count.bad {
    font-weight: 700;
  }
  .empty {
    padding: 0.5rem 1rem;
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .more {
      opacity: 1;
    }
    .item {
      min-height: 2.375rem;
      font-size: var(--fs-lg);
    }
  }
</style>
