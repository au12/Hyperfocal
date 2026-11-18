<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { ImageCache } from '../lib/viewer/loader';
  import { compareCtl, toggleFullscreen } from '../lib/ctl';
  import { keysFor, prettyKey } from '../lib/keys';
  import ComparePane, { type PaneView, type PaneCtl } from './ComparePane.svelte';
  import Icon from './Icon.svelte';

  const cache = new ImageCache(16);
  const ids = $derived(store.compare ?? []);
  const n = $derived(ids.length);

  /** Synced: every pane shows the same zoom (100% = 1:1 pixels) at the same point of
   * its photo. Otherwise each pane has its own view. */
  let sync = $state(true);
  const FIT: PaneView = { fit: true, scale: 1, nx: 0.5, ny: 0.5 };
  let shared = $state<PaneView>({ ...FIT });
  let own = $state<PaneView[]>([{ ...FIT }, { ...FIT }, { ...FIT }, { ...FIT }]);
  let ctls = $state<(PaneCtl | undefined)[]>([]);

  let box: HTMLDivElement | undefined = $state();
  let portrait = $state(false);

  const viewOf = (k: number) => (sync ? shared : own[k]);

  function setView(k: number, v: PaneView) {
    if (sync) shared = v;
    else own[k] = v;
  }

  function toggleSync() {
    if (sync) own = own.map(() => ({ ...shared }));
    else shared = { ...own[store.compareActive] };
    sync = !sync;
  }

  function fitAll() {
    shared = { ...FIT };
    own = own.map(() => ({ ...FIT }));
  }

  const active = () => ctls[store.compareActive];

  onMount(() => {
    compareCtl.zoomToggle = () => active()?.zoomToggle();
    compareCtl.fit = fitAll;
    compareCtl.zoomIn = () => active()?.zoomBy(1.5);
    compareCtl.zoomOut = () => active()?.zoomBy(1 / 1.5);
    const ro = new ResizeObserver(() => {
      if (box) portrait = box.clientHeight > box.clientWidth * 1.1;
    });
    if (box) ro.observe(box);
    return () => {
      ro.disconnect();
      cache.clear();
      compareCtl.zoomToggle = compareCtl.fit = compareCtl.zoomIn = compareCtl.zoomOut = () => {};
    };
  });

  // Pane layout: side by side (stacked in portrait), 2 x 2 for four.
  const cols = $derived(n === 4 ? 2 : portrait ? 1 : n);
  const rows = $derived(n === 4 ? 2 : portrait ? n : 1);
  const k = (a: Parameters<typeof keysFor>[0]) => keysFor(a, store.settings.keymap).slice(0, 1).map(prettyKey).join('');
</script>

<div class="compare" role="dialog" aria-label="Compare photos">
  <header class="top">
    <button class="btn ghost icon" onclick={() => store.closeCompare()} title="Back to the grid (Esc)" aria-label="Back"><Icon name="arrow-left" /></button>
    <div class="title">
      <div class="fname">Compare</div>
      <div class="sub dim">{n} photos{#if !store.mobile}{` · ${k('next')} ${k('prev')} active photo · ↑ ↓ step it through the folder`}{/if}</div>
    </div>
    <div class="tools">
      <div class="seg">
        <button class:on={store.viewer.source !== 'raw'} onclick={() => store.setViewer('source', 'jpeg')}>JPEG</button>
        <button class:on={store.viewer.source === 'raw'} onclick={() => store.setViewer('source', 'raw')}>RAW</button>
      </div>
      <span class="vsep"></span>
      <button class="btn sm" class:on={sync} onclick={toggleSync} aria-pressed={sync} title="Zoom and pan all photos together">
        <Icon name="link" />{store.mobile ? '' : 'Sync'}
      </button>
      <button class="btn sm ghost" onclick={fitAll} title="Fit (Ctrl+0)">Fit</button>
      <button class="btn sm ghost" onclick={() => active()?.zoomToggle()} title="100% ({k('zoomToggle')})">100%</button>
    </div>
    <div class="right">
      {#if !store.mobile}
        <button class="btn ghost icon" onclick={toggleFullscreen} title="Full screen (F)" aria-label="Full screen"><Icon name="fullscreen" /></button>
      {/if}
    </div>
  </header>
  <div class="panes" bind:this={box} style:grid-template-columns="repeat({cols}, minmax(0, 1fr))" style:grid-template-rows="repeat({rows}, minmax(0, 1fr))">
    {#each ids as id, j (j)}
      <ComparePane
        {id}
        {cache}
        active={j === store.compareActive}
        view={viewOf(j)}
        onview={(v) => setView(j, v)}
        onactivate={() => store.setCompareActive(j)}
        onremove={() => store.compareRemove(j)}
        bind:ctl={ctls[j]}
      />
    {/each}
  </div>
</div>

<style>
  .compare {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    flex-direction: column;
    background: var(--viewer-bg);
    color: #e8edf2;
    --text: #e8edf2;
    --text-2: #aab6c2;
    --text-3: #8396a8;
  }
  .compare :global(.btn) {
    --surface: #1c242c;
    --surface-2: #26303a;
    --border-strong: #3a4a5a;
  }
  .top {
    height: 3.25rem;
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0 0.625rem;
    padding-top: env(safe-area-inset-top);
    box-sizing: content-box;
    background: #141a20;
    border-bottom: 1px solid #222b35;
  }
  .title {
    min-width: 0;
    flex: 0 1 22rem;
  }
  .fname {
    font-weight: 700;
  }
  .sub {
    font-size: var(--fs-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tools {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.375rem;
    justify-content: center;
  }
  .tools .seg > button {
    height: 1.75rem;
    font-size: var(--fs-sm);
  }
  .tools :global(.btn.on) {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .vsep {
    width: 1px;
    height: 1.25rem;
    background: #2e3742;
    margin: 0 0.25rem;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    margin-left: auto;
  }
  .panes {
    flex: 1;
    min-height: 0;
    display: grid;
    gap: 2px;
    background: #0b0e11;
  }
  @media (max-width: 760px) {
    .title {
      flex: 0 1 auto;
    }
    .tools {
      justify-content: flex-end;
    }
    .right {
      display: none;
    }
  }
</style>
