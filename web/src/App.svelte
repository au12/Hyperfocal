<script lang="ts">
  import { onMount } from 'svelte';
  import { store, LABELS, PANEL } from './lib/store.svelte';
  import { actionFor, type Action } from './lib/keys';
  import { gridCtl, uiCtl, viewerCtl, compareCtl, toggleFullscreen } from './lib/ctl';
  import { fullscreen } from './lib/fullscreen.svelte';
  import Shell from './components/Shell.svelte';
  import Sidebar from './components/Sidebar.svelte';
  import Toolbar from './components/Toolbar.svelte';
  import Grid from './components/Grid.svelte';
  import Loupe from './components/Loupe.svelte';
  import Compare from './components/Compare.svelte';
  import InfoPanel from './components/InfoPanel.svelte';
  import MapView from './components/MapView.svelte';
  import Toasts from './components/Toasts.svelte';
  import Login from './components/Login.svelte';
  import SettingsDialog from './components/SettingsDialog.svelte';
  import HelpDialog from './components/HelpDialog.svelte';
  import JournalDialog from './components/JournalDialog.svelte';
  import LocationDialog from './components/LocationDialog.svelte';
  import KeywordManager from './components/KeywordManager.svelte';
  import GeotagDialog from './components/GeotagDialog.svelte';
  import IntegrityDialog from './components/IntegrityDialog.svelte';
  import DuplicatesDialog from './components/DuplicatesDialog.svelte';
  import BulkBar from './components/BulkBar.svelte';
  import MobileNav from './components/MobileNav.svelte';
  import PeopleView from './components/PeopleView.svelte';
  import SeriesView from './components/SeriesView.svelte';
  import AlbumPicker from './components/AlbumPicker.svelte';
  import ImmichDialog from './components/ImmichDialog.svelte';
  import PanelEdge from './components/PanelEdge.svelte';
  import ContextMenu from './components/ContextMenu.svelte';
  import { albums } from './lib/albums.svelte';
  import { cull } from './lib/cull.svelte';

  let drawer = $state(false);

  // Desktop side panels: shown, and how wide (the edge between a panel and the photos
  // resizes it; its tab hides it). The widths stay within the window in CSS.
  const sidebarOpen = $derived(!store.mobile && store.panels.sidebar && !store.panels.hidden);
  const infoOpen = $derived(!store.mobile && store.panels.info && !store.panels.hidden && store.view === 'grid');
  const SIDEBAR_VW = 40;
  const INFO_VW = 45;
  const sidebarW = $derived(`clamp(${PANEL.sidebar.min}px, ${store.panels.sidebarW || PANEL.sidebar.w}px, ${SIDEBAR_VW}vw)`);
  // In a small window the info panel gives way first, so the photos keep PANEL.content px.
  const infoW = $derived(
    `clamp(${PANEL.info.min}px, ${store.panels.infoW || PANEL.info.w}px, min(${INFO_VW}vw, 100vw - ${sidebarOpen ? 'var(--sidebar-w)' : '0px'} - ${PANEL.content}px))`,
  );

  function togglePanel(k: 'sidebar' | 'info') {
    if (store.panels[k] && !store.panels.hidden) store.panels[k] = false;
    else {
      store.panels[k] = true;
      store.panels.hidden = false;
    }
    store.persist();
  }

  onMount(() => {
    store.init();
  });

  // Info panel follows the grid cursor (debounced).
  let cursorTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const id = store.list.id[store.cursor];
    void store.rev;
    if (store.loupe || id == null) return;
    clearTimeout(cursorTimer);
    cursorTimer = setTimeout(() => {
      if (!store.loupe && store.detail?.id !== id) store.loadDetail();
    }, 120);
  });

  const EV_STEP = 1 / 3;

  /** Keys that move the single view to another photo, and by how many. Held down, each
   * repeat waits until the photo it moves to can show (see viewerCtl.canStep), so none
   * goes by unseen. */
  const STEPS: Partial<Record<Action, number>> = {
    next: 1, prev: -1, next2: 2, prev2: -2, next3: 3, prev3: -3, pageDown: 10, pageUp: -10,
    selectNext: 1, selectPrev: -1, selectRight: 1, selectLeft: -1, selectPageDown: 10, selectPageUp: -10,
  };
  /** The step key being held: letting go of it ends scrubbing at once. */
  let heldStep = '';

  function onKeyUp(e: KeyboardEvent) {
    if (!heldStep || e.code !== heldStep) return;
    heldStep = '';
    store.settle();
  }

  /** Keys in the compare view: arrows pick the active pane or step its photo, zoom keys
   * zoom, rating and flag keys apply to the active photo (via store.targets()). */
  function runCompare(a: Action): boolean | null {
    const c = store.compare!;
    switch (a) {
      case 'next': store.setCompareActive((store.compareActive + 1) % c.length); return true;
      case 'prev': store.setCompareActive((store.compareActive + c.length - 1) % c.length); return true;
      case 'down': store.compareStep(1); return true;
      case 'up': store.compareStep(-1); return true;
      // Esc never leaves full screen here (see lib/fullscreen.svelte.ts).
      case 'escape': store.closeCompare(); return true;
      case 'compare': case 'toggleGrid': store.closeCompare(); return true;
      case 'open': {
        const i = store.index.get(c[store.compareActive]);
        store.closeCompare();
        if (i != null) store.openLoupe(i);
        return true;
      }
      case 'zoomToggle': compareCtl.zoomToggle(); return true;
      case 'zoomCycle': compareCtl.zoomCycle(); return true;
      case 'fit': compareCtl.fit(); return true;
      case 'zoomIn': compareCtl.zoomIn(); return true;
      case 'zoomOut': compareCtl.zoomOut(); return true;
      // Moving through the folder, selection and side panels do not apply here.
      case 'next2': case 'prev2': case 'next3': case 'prev3': case 'first': case 'last': case 'pageDown': case 'pageUp':
      case 'selectToggle': case 'selectNext': case 'selectPrev': case 'selectRight': case 'selectLeft': case 'selectDown': case 'selectUp':
      case 'selectFirst': case 'selectLast': case 'selectPageDown': case 'selectPageUp': case 'selectAll': case 'selectNone': case 'selectInvert': case 'map':
        return true;
    }
    return null;
  }

  function run(a: Action, e: KeyboardEvent): boolean {
    const v = store.viewer;
    const inLoupe = store.loupe;
    const focus = inLoupe ? store.current : store.cursor;
    if (store.compare) {
      const r = runCompare(a);
      if (r != null) return r;
    }
    // The People view has no photo under the cursor: only a few keys apply there.
    if (store.view === 'people' && !inLoupe) {
      switch (a) {
        case 'people': case 'toggleGrid': case 'escape': store.showPhotos(); return true;
        case 'map': store.openLocations(); return true;
        case 'help': store.dialog = 'help'; return true;
        case 'folders': togglePanel('sidebar'); return true;
        case 'undo': store.undo(); return true;
        default: return false;
      }
    }
    // Locations: the map. Esc clears the search, then goes back to the photos, like M
    // and G. No photo is under a cursor here, so nothing rates, moves or selects one.
    if (store.view === 'map' && !inLoupe) {
      switch (a) {
        case 'map': case 'toggleGrid': store.showPhotos(); return true;
        case 'escape':
          if (store.query) store.search('');
          else store.showPhotos();
          return true;
        case 'people': store.openPeople(); return true;
        case 'help': store.dialog = 'help'; return true;
        case 'search': uiCtl.focusSearch(); return true;
        case 'undo': store.undo(); return true;
        case 'folders': togglePanel('sidebar'); return true;
        case 'hidePanels': store.panels.hidden = !store.panels.hidden; return true;
        case 'fullscreen': toggleFullscreen(); return true;
        default: return false;
      }
    }
    // Reviewing a series: ←/→ stay within it (store.go), ↑/↓ move to the series before
    // and after, Esc goes back (from the single view: to the series).
    if (cull.focus != null) {
      switch (a) {
        case 'up': case 'pageUp': cull.step(-1); return true;
        case 'down': case 'pageDown': cull.step(1); return true;
        case 'keepBest': cull.keepBestHere(); return true;
        case 'review': return true;
        case 'stack':
          if (inLoupe) store.closeLoupe();
          cull.leave();
          return true;
        case 'escape':
          if (inLoupe) store.closeLoupe();
          else cull.leave();
          return true;
        case 'selectDown': case 'selectUp': return true;
        case 'map': case 'people': case 'search':
          cull.leave();
          break;
      }
    }
    const step = STEPS[a];
    if (e.repeat && inLoupe && step) {
      heldStep = e.code;
      if (!viewerCtl.canStep(step)) return true;
    }
    switch (a) {
      case 'next': store.go(1); break;
      case 'prev': store.go(-1); break;
      case 'next2': store.go(2); break;
      case 'prev2': store.go(-2); break;
      case 'next3': store.go(3); break;
      case 'prev3': store.go(-3); break;
      case 'first': store.goTo(0); break;
      case 'last': store.goTo(store.list.id.length - 1); break;
      case 'pageDown': inLoupe ? store.go(10) : gridCtl.page(1); break;
      case 'pageUp': inLoupe ? store.go(-10) : gridCtl.page(-1); break;
      case 'up': if (inLoupe) return false; gridCtl.vertical(-1); break;
      case 'down': if (inLoupe) return false; gridCtl.vertical(1); break;
      case 'open':
        if (inLoupe) break;
        // Enter on a folded series reviews it.
        if (cull.on() && cull.focus == null && store.list.sk?.[store.cursor] && !cull.open.has(store.list.sk[store.cursor])) cull.review();
        else store.openLoupe(store.cursor);
        break;
      case 'review': cull.review(); break;
      case 'escape':
        // Esc does not leave full screen: on the library page a quick double press does.
        if (inLoupe) store.closeLoupe();
        else fullscreen.libraryEsc(e, () => {
          if (store.selected.size) store.clearSelection();
          else if (store.query) store.search('');
          else return false;
          return true;
        });
        break;
      case 'toggleGrid': inLoupe ? store.closeLoupe() : store.openLoupe(store.cursor); break;
      case 'compare': store.openCompare(); break;
      case 'zoomToggle': if (!inLoupe) store.openLoupe(store.cursor); else viewerCtl.zoomToggle(); break;
      case 'zoomCycle': if (!inLoupe) store.openLoupe(store.cursor); else viewerCtl.zoomCycle(); break;
      case 'fit': viewerCtl.fit(); break;
      case 'zoomIn': viewerCtl.zoomIn(); break;
      case 'zoomOut': viewerCtl.zoomOut(); break;
      case 'fullscreen': toggleFullscreen(); break;
      case 'source': store.setViewer('source', v.source === 'raw' ? 'jpeg' : 'raw'); break;
      case 'peaking': store.setViewer('peaking', ((v.peaking + 1) % 4) as 0 | 1 | 2 | 3); break;
      case 'sharpen': store.setViewer('sharpen', ((v.sharpen + 1) % 3) as 0 | 1 | 2); break;
      case 'shadows': store.toggleTone('shadows'); break;
      case 'highlights': store.toggleTone('highlights'); break;
      case 'linear': store.toggleTone('linear'); break;
      case 'invert': store.setViewer('invert', !v.invert); break;
      case 'over': store.setViewer('over', !v.over); break;
      case 'under': store.setViewer('under', !v.under); break;
      case 'wb': {
        const order = ['asshot', 'daylight', 'auto'] as const;
        store.setViewer('wb', order[(order.indexOf(v.wb) + 1) % 3]);
        if (v.source !== 'raw') store.setViewer('source', 'raw');
        store.toast(`White balance: ${({ asshot: 'As shot', daylight: 'Daylight', auto: 'Auto' })[store.viewer.wb]}`, 'info', undefined, 1500);
        break;
      }
      case 'evToggle': store.setViewer('evOn', !v.evOn); break;
      case 'evReset': store.setViewer('ev', 0); break;
      case 'evUp': store.bumpEv(EV_STEP); break;
      case 'evDown': store.bumpEv(-EV_STEP); break;
      case 'evUpBig': store.bumpEv(1); break;
      case 'evDownBig': store.bumpEv(-1); break;
      case 'rate0': store.rate(0); break;
      case 'rate1': store.rate(1); break;
      case 'rate2': store.rate(2); break;
      case 'rate3': store.rate(3); break;
      case 'rate4': store.rate(4); break;
      case 'rate5': store.rate(5); break;
      case 'rateReject': store.rate(-1); break;
      case 'rateUp': store.bumpRating(1); break;
      case 'rateDown': store.bumpRating(-1); break;
      case 'labelRed': store.label('red'); break;
      case 'labelYellow': store.label('yellow'); break;
      case 'labelGreen': store.label('green'); break;
      case 'labelBlue': store.label('blue'); break;
      case 'labelPurple': store.label('purple'); break;
      case 'pick': store.togglePick(1); break;
      case 'unflag': store.patch(store.targets(), { pick: 0 }); break;
      case 'reject': store.reject(); break;
      case 'editTitle': store.panels.info = true; uiCtl.editTitle(); break;
      case 'keywords': store.panels.info = true; uiCtl.addKeyword(); break;
      case 'location': store.openLocation(); break;
      case 'albumAdd': albums.addToTarget(); break;
      case 'albumPick': albums.pick(); break;
      case 'albumRemove':
        if (albums.currentId == null) store.toast('Open an album first: this takes photos out of the album shown', 'info');
        else albums.remove();
        break;
      case 'selectToggle': store.select(focus, 'toggle'); break;
      case 'selectNext': store.select(focus, 'toggle'); store.go(1, true); break;
      case 'selectPrev': store.go(-1, true); store.select(inLoupe ? store.current : store.cursor, 'toggle'); break;
      case 'selectRight': store.extendTo(focus + 1); break;
      case 'selectLeft': store.extendTo(focus - 1); break;
      case 'selectFirst': store.extendTo(0); break;
      case 'selectLast': store.extendTo(store.list.id.length - 1); break;
      case 'selectDown': case 'selectUp': {
        if (inLoupe) return false;
        const to = gridCtl.rowNeighbor(store.cursor, a === 'selectDown' ? 1 : -1);
        if (to != null) store.extendTo(to);
        break;
      }
      case 'selectPageDown': case 'selectPageUp': {
        const d = a === 'selectPageDown' ? 1 : -1;
        const to = inLoupe ? Math.max(0, Math.min(store.list.id.length - 1, focus + d * 10)) : gridCtl.pageTarget(d);
        if (to != null) store.extendTo(to);
        break;
      }
      case 'selectAll': store.selectAll(); break;
      case 'selectNone': store.clearSelection(); break;
      case 'selectInvert': store.invertSelection(); break;
      case 'rotateCcw': store.rotate(-90); break;
      case 'rotateCw': store.rotate(90); break;
      case 'rotate180': store.rotate(180); break;
      case 'histogram': store.panels.histogram = !store.panels.histogram; store.persist(); break;
      case 'info': if (inLoupe) { store.panels.info = !store.panels.info; store.persist(); } else togglePanel('info'); break;
      case 'filmstrip': store.panels.filmstrip = !store.panels.filmstrip; store.persist(); break;
      case 'folders': if (inLoupe) { store.panels.sidebar = !store.panels.sidebar; store.persist(); } else togglePanel('sidebar'); break;
      case 'hidePanels': store.panels.hidden = !store.panels.hidden; break;
      case 'help': store.dialog = 'help'; break;
      case 'search': uiCtl.focusSearch(); break;
      case 'undo': store.undo(); break;
      // From a photo opened on the map, M goes back to the map.
      case 'map': if (store.view === 'map') store.closeLoupe(); else store.openLocations(); break;
      case 'faces': store.panels.faces = !store.panels.faces; store.persist(); break;
      case 'people': store.openPeople(); break;
      case 'cull': cull.toggle(); break;
      case 'stack': cull.toggleStackAt(); break;
      case 'closeups': store.panels.closeups = !store.panels.closeups; store.persist(); break;
      case 'keepBest': {
        const id = store.list.id[inLoupe ? store.current : store.cursor];
        if (id == null) break;
        cull.get(id).then((c) => {
          if (c?.series) cull.keepBest(c);
          else store.toast('Not part of a series', 'info', undefined, 1800);
        });
        break;
      }
    }
    return true;
  }

  function onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
    // Only Esc presses on the library page itself count towards the quick double press.
    if (e.key === 'Escape' && (typing || store.dialog || albums.picking || store.needLogin || store.compare || store.loupe || store.view !== 'grid')) fullscreen.resetEsc();
    if (typing) {
      if (e.key === 'Escape') (t as HTMLElement).blur();
      return;
    }
    if (store.dialog) {
      if (e.key === 'Escape') {
        store.dialog = null;
        e.preventDefault();
      }
      return;
    }
    if (albums.picking) {
      if (e.key === 'Escape') {
        albums.picking = null;
        e.preventDefault();
      }
      return;
    }
    if (store.needLogin) return;
    const a = actionFor(e, store.settings.keymap, store.keyOverrides());
    if (!a) return;
    if (run(a, e)) e.preventDefault();
  }

  const labelCount = LABELS.length;
  void labelCount;
</script>

<svelte:window onkeydown={onKey} onkeyup={onKeyUp} />

{#if store.needLogin}
  <Login />
{:else}
  <div class="app" class:mobile={store.mobile} style:--sidebar-w={store.mobile ? undefined : sidebarW} style:--panel-w={store.mobile ? undefined : infoW}>
    <Shell onmenu={() => (drawer = !drawer)} />
    <div class="main">
      {#if sidebarOpen}
        <Sidebar />
      {/if}
      {#if !store.mobile}
        <PanelEdge
          side="left"
          open={sidebarOpen}
          width={store.panels.sidebarW || PANEL.sidebar.w}
          min={PANEL.sidebar.min}
          max={PANEL.sidebar.max}
          def={PANEL.sidebar.w}
          maxVw={SIDEBAR_VW}
          label="sidebar"
          hint={store.keyHint('folders')}
          onresize={(w) => (store.panels.sidebarW = w)}
          ondone={store.persist}
          ontoggle={() => togglePanel('sidebar')}
        />
      {/if}
      <section class="content" style:--edge-l={!store.mobile && !sidebarOpen ? '0.5rem' : undefined} style:--edge-r={!store.mobile && store.view === 'grid' && !infoOpen ? '0.375rem' : undefined}>
        {#if store.view === 'people'}
          <PeopleView />
        {:else}
          {#if cull.focus != null && store.view === 'grid'}
            <SeriesView />
          {:else}
            <Toolbar />
            {#if store.view === 'map'}
              <MapView />
            {:else}
              <Grid />
            {/if}
            <BulkBar />
          {/if}
        {/if}
      </section>
      {#if !store.mobile && store.view === 'grid'}
        <PanelEdge
          side="right"
          open={infoOpen}
          width={store.panels.infoW || PANEL.info.w}
          min={PANEL.info.min}
          max={PANEL.info.max}
          def={PANEL.info.w}
          maxVw={INFO_VW}
          label="info panel"
          hint={store.keyHint('info')}
          onresize={(w) => (store.panels.infoW = w)}
          ondone={store.persist}
          ontoggle={() => togglePanel('info')}
        />
      {/if}
      {#if infoOpen}
        <aside class="info">
          <InfoPanel />
        </aside>
      {/if}
    </div>
    {#if store.mobile}
      <MobileNav />
    {/if}
  </div>

  {#if store.mobile && drawer}
    <div class="drawer-scrim" onclick={() => (drawer = false)} role="presentation"></div>
    <div class="drawer">
      <Sidebar onpick={() => (drawer = false)} />
    </div>
  {/if}

  {#if store.compare}
    <Compare />
  {:else if store.loupe}
    <Loupe />
  {/if}

  {#if albums.picking}<AlbumPicker />{/if}
  {#if store.dialog === 'immich'}<ImmichDialog />{/if}
  {#if store.dialog === 'settings'}<SettingsDialog />{/if}
  {#if store.dialog === 'help'}<HelpDialog />{/if}
  {#if store.dialog === 'journal'}<JournalDialog />{/if}
  {#if store.dialog === 'location'}<LocationDialog />{/if}
  {#if store.dialog === 'keywords'}<KeywordManager />{/if}
  {#if store.dialog === 'geotag'}<GeotagDialog />{/if}
  {#if store.dialog === 'integrity'}<IntegrityDialog />{/if}
  {#if store.dialog === 'duplicates'}<DuplicatesDialog />{/if}
{/if}
<ContextMenu />
<Toasts />

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100dvh;
    overflow: hidden;
  }
  .main {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    position: relative;
  }
  .info {
    width: var(--panel-w);
    flex: none;
    border-left: 1px solid var(--border);
    background: var(--surface);
    overflow: auto;
  }
  .drawer-scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 70;
    animation: fade 0.15s;
  }
  .drawer {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    width: min(20rem, 86vw);
    z-index: 71;
    background: var(--surface);
    box-shadow: var(--shadow-2);
    animation: slide 0.18s var(--ease);
    display: flex;
    padding-top: env(safe-area-inset-top);
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
  }
</style>
