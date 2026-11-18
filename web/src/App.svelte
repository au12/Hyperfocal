<script lang="ts">
  import { onMount } from 'svelte';
  import { store, LABELS } from './lib/store.svelte';
  import { actionFor, type Action } from './lib/keys';
  import { gridCtl, uiCtl, viewerCtl, toggleFullscreen } from './lib/ctl';
  import Shell from './components/Shell.svelte';
  import Sidebar from './components/Sidebar.svelte';
  import Toolbar from './components/Toolbar.svelte';
  import Grid from './components/Grid.svelte';
  import Loupe from './components/Loupe.svelte';
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
  import BulkBar from './components/BulkBar.svelte';
  import MobileNav from './components/MobileNav.svelte';

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

  function run(a: Action, e: KeyboardEvent): boolean {
    const v = store.viewer;
    const inLoupe = store.loupe;
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
      case 'zoomToggle': if (!inLoupe) store.openLoupe(store.cursor); else viewerCtl.zoomToggle(); break;
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
      case 'evUp': store.setViewer('ev', Math.min(5, +(v.ev + EV_STEP).toFixed(2))); store.setViewer('evOn', true); break;
      case 'evDown': store.setViewer('ev', Math.max(-5, +(v.ev - EV_STEP).toFixed(2))); store.setViewer('evOn', true); break;
      case 'evUpBig': store.setViewer('ev', Math.min(5, v.ev + 1)); store.setViewer('evOn', true); break;
      case 'evDownBig': store.setViewer('ev', Math.max(-5, v.ev - 1)); store.setViewer('evOn', true); break;
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
      case 'selectToggle': store.select(inLoupe ? store.current : store.cursor, 'toggle'); break;
      case 'selectNext': store.select(inLoupe ? store.current : store.cursor, 'toggle'); store.go(1); break;
      case 'selectPrev': store.go(-1); store.select(inLoupe ? store.current : store.cursor, 'toggle'); break;
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
    const a = actionFor(e, store.settings.keymap);
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
        <Toolbar />
        {#if store.view === 'map'}
          <MapView />
        {:else}
          <Grid />
        {/if}
        <BulkBar />
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

  {#if store.loupe}
    <Loupe />
  {/if}

  {#if store.dialog === 'settings'}<SettingsDialog />{/if}
  {#if store.dialog === 'help'}<HelpDialog />{/if}
  {#if store.dialog === 'journal'}<JournalDialog />{/if}
  {#if store.dialog === 'location'}<LocationDialog />{/if}
  {#if store.dialog === 'keywords'}<KeywordManager />{/if}
  {#if store.dialog === 'geotag'}<GeotagDialog />{/if}
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
