<script lang="ts">
  import { onMount } from 'svelte';
  import { store, LABELS } from './lib/store.svelte';
  import { actionFor, type Action } from './lib/keys';
  import { gridCtl, uiCtl, viewerCtl, compareCtl, toggleFullscreen } from './lib/ctl';
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

  let drawer = $state(false);

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

  /** Keys in the compare view: arrows pick the active pane or step its photo, zoom keys
   * zoom, rating and flag keys apply to the active photo (via store.targets()). */
  function runCompare(a: Action): boolean | null {
    const c = store.compare!;
    switch (a) {
      case 'next': store.setCompareActive((store.compareActive + 1) % c.length); return true;
      case 'prev': store.setCompareActive((store.compareActive + c.length - 1) % c.length); return true;
      case 'down': store.compareStep(1); return true;
      case 'up': store.compareStep(-1); return true;
      case 'escape':
        if (document.fullscreenElement) toggleFullscreen();
        else store.closeCompare();
        return true;
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
        case 'people': case 'toggleGrid': case 'escape': store.view = 'grid'; return true;
        case 'map': store.view = 'map'; return true;
        case 'help': store.dialog = 'help'; return true;
        case 'folders': store.panels.sidebar = !store.panels.sidebar; store.persist(); return true;
        case 'undo': store.undo(); return true;
        default: return false;
      }
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
      case 'open': if (!inLoupe) store.openLoupe(store.cursor); break;
      case 'escape':
        if (document.fullscreenElement) toggleFullscreen();
        else if (inLoupe) store.closeLoupe();
        else if (store.selected.size) store.clearSelection();
        else if (store.query) store.search('');
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
      case 'info': store.panels.info = !store.panels.info; store.persist(); break;
      case 'filmstrip': store.panels.filmstrip = !store.panels.filmstrip; store.persist(); break;
      case 'folders': store.panels.sidebar = !store.panels.sidebar; store.persist(); break;
      case 'hidePanels': store.panels.hidden = !store.panels.hidden; break;
      case 'help': store.dialog = 'help'; break;
      case 'search': uiCtl.focusSearch(); break;
      case 'undo': store.undo(); break;
      case 'map': store.view = store.view === 'map' ? 'grid' : 'map'; store.loupe = false; break;
      case 'faces': store.panels.faces = !store.panels.faces; store.persist(); break;
      case 'people': store.openPeople(); break;
    }
    void e;
    return true;
  }

  function onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
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
    if (store.needLogin) return;
    const a = actionFor(e, store.settings.keymap, store.keyOverrides());
    if (!a) return;
    if (run(a, e)) e.preventDefault();
  }

  const labelCount = LABELS.length;
  void labelCount;
</script>

<svelte:window onkeydown={onKey} />

{#if store.needLogin}
  <Login />
{:else}
  <div class="app" class:mobile={store.mobile}>
    <Shell onmenu={() => (drawer = !drawer)} />
    <div class="main">
      {#if !store.mobile && store.panels.sidebar && !store.panels.hidden}
        <Sidebar />
      {/if}
      <section class="content">
        {#if store.view === 'people'}
          <PeopleView />
        {:else}
          <Toolbar />
          {#if store.view === 'map'}
            <MapView />
          {:else}
            <Grid />
          {/if}
          <BulkBar />
        {/if}
      </section>
      {#if !store.mobile && store.panels.info && !store.panels.hidden && store.view === 'grid'}
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

  {#if store.dialog === 'settings'}<SettingsDialog />{/if}
  {#if store.dialog === 'help'}<HelpDialog />{/if}
  {#if store.dialog === 'journal'}<JournalDialog />{/if}
  {#if store.dialog === 'location'}<LocationDialog />{/if}
  {#if store.dialog === 'keywords'}<KeywordManager />{/if}
  {#if store.dialog === 'geotag'}<GeotagDialog />{/if}
  {#if store.dialog === 'integrity'}<IntegrityDialog />{/if}
  {#if store.dialog === 'duplicates'}<DuplicatesDialog />{/if}
{/if}
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
