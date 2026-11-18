<script lang="ts">
  import { onMount } from 'svelte';
  import { store, applyTheme } from '../lib/store.svelte';
  import { uiCtl } from '../lib/ctl';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';

  let { onmenu }: { onmenu?: () => void } = $props();
  let input: HTMLInputElement | undefined = $state();
  let q = $state(store.query);
  let mobileSearch = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    uiCtl.focusSearch = () => {
      mobileSearch = true;
      requestAnimationFrame(() => input?.focus());
      input?.select();
    };
  });

  $effect(() => {
    if (store.query === '' && q !== '' && document.activeElement !== input) q = '';
  });

  function onInput() {
    clearTimeout(timer);
    timer = setTimeout(() => store.search(q), 250);
  }

  function clear() {
    q = '';
    store.search('');
    mobileSearch = false;
  }

  function toggleTheme() {
    const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
    store.settings.theme = next;
    applyTheme(next);
    store.persist();
  }

  const p = $derived(store.status?.progress);
  const indexing = $derived(p && p.phase !== 'idle' && p.phase !== 'starting');
  const folderName = $derived(store.query ? 'Search' : (store.folders.get(store.folderId ?? -1)?.name ?? 'Hyperfocal'));
  const running = $derived(store.runningJobs());
</script>

<header class="shell">
  {#if store.mobile}
    <button class="btn ghost icon" onclick={onmenu} aria-label="Folders"><Icon name="menu" /></button>
    {#if mobileSearch || store.query}
      <div class="search grow">
        <Icon name="search" size={16} />
        <input bind:this={input} bind:value={q} oninput={onInput} placeholder="Search photos" enterkeyhint="search" />
        <button class="clear" onclick={clear} aria-label="Clear search"><Icon name="x" size={16} /></button>
      </div>
    {:else}
      <div class="title grow ellipsis">{folderName}</div>
      <button class="btn ghost icon" onclick={() => uiCtl.focusSearch()} aria-label="Search"><Icon name="search" /></button>
    {/if}
    <button class="btn ghost icon" onclick={() => (store.dialog = 'settings')} aria-label="Settings"><Icon name="settings" /></button>
  {:else}
    <div class="brand">
      <img class="logo" src="./logo.png" alt="" width="32" height="32" />
      <span class="name">Hyperfocal</span>
    </div>
    <div class="search">
      <Icon name="search" size={16} />
      <input bind:this={input} bind:value={q} oninput={onInput} placeholder="Search: file name, folder, title, caption, keyword, place, camera" />
      {#if q}<button class="clear" onclick={clear} aria-label="Clear search"><Icon name="x" size={16} /></button>{:else}<kbd>/</kbd>{/if}
    </div>
    <div class="right">
      {#if indexing}
        <div class="status" title={p?.current ?? ''}>
          <span class="spinner"></span>
          <span class="tnum">{p?.phase === 'walking' ? 'Reading folders…' : `Indexing ${p?.folders_done}/${p?.folders_total}`}</span>
        </div>
      {:else if p && p.thumbs_pending > 0}
        <div class="status" title="Building thumbnails in the background">
          <span class="spinner"></span><span class="tnum">{p.thumbs_pending.toLocaleString()} thumbnails</span>
        </div>
      {/if}
      {#if p && !p.library_ok}
        <span class="warn" title={p.message ?? ''}><Icon name="info" size={16} /> Library offline</span>
      {/if}
      {#if store.status?.readonly}
        <span class="ro" title="The photo library is mounted read-only or HYPERFOCAL_READONLY is set"><Icon name="lock" size={14} /> Read-only</span>
      {/if}
      {#if running.length}
        {@const j = running[0]}
        <div class="status" title={running.map((r) => r.label).join('\n')}>
          <span class="spinner"></span>
          <span class="tnum">{running.length > 1 ? `${running.length} changes running` : `${j.label}: ${j.done.toLocaleString()} of ${j.total.toLocaleString()}`}</span>
        </div>
      {/if}
      {#if store.busy > 0}<span class="spinner small" title="Saving…"></span>{/if}
      <button class="btn ghost icon" onclick={() => (store.dialog = 'journal')} title="History & undo (Ctrl+Z)"><Icon name="history" /></button>
      <Popover align="right">
        {#snippet trigger(toggle, open)}
          <button class="btn ghost icon" class:on={open} onclick={toggle} title="Library tools"><Icon name="wrench" /></button>
        {/snippet}
        {#snippet children(close)}
          <button class="menu-item" onclick={() => { close(); store.dialog = 'keywords'; }}><Icon name="tags" />Keywords</button>
          <button class="menu-item" onclick={() => { close(); store.dialog = 'geotag'; }}><Icon name="route" />Geotag from a track</button>
        {/snippet}
      </Popover>
      <button class="btn ghost icon" onclick={toggleTheme} title="Light / dark"><Icon name={store.settings.theme === 'light' ? 'moon' : 'sun'} /></button>
      <button class="btn ghost icon" onclick={() => (store.dialog = 'help')} title="Keyboard shortcuts (F1)"><Icon name="keyboard" /></button>
      <button class="btn ghost icon" onclick={() => (store.dialog = 'settings')} title="Settings"><Icon name="settings" /></button>
    </div>
  {/if}
</header>

<style>
  .shell {
    height: var(--shell-h);
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0 0.75rem;
    padding-top: env(safe-area-inset-top);
    box-sizing: content-box;
    background: var(--shell);
    border-bottom: 1px solid var(--border);
    position: relative;
    z-index: 20;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    width: calc(var(--sidebar-w) - 1.5rem);
    flex: none;
  }
  .logo {
    width: 2rem;
    height: 2rem;
    flex: none;
    filter: drop-shadow(0 2px 6px rgba(0, 112, 242, 0.3));
  }
  .name {
    font-weight: 700;
    font-size: 1.0625rem;
    letter-spacing: -0.01em;
  }
  .title {
    font-weight: 700;
    font-size: 1rem;
  }
  .search {
    flex: 1;
    max-width: 40rem;
    height: 2.25rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0 0.75rem;
    border-radius: var(--r);
    background: var(--field);
    border: 1px solid var(--border-strong);
    color: var(--text-3);
    margin: 0 auto;
  }
  .search:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text);
    height: 100%;
  }
  .search input::placeholder {
    color: var(--text-3);
  }
  .clear {
    border: 0;
    background: none;
    color: var(--text-2);
    cursor: pointer;
    display: flex;
    padding: 0.25rem;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    flex: none;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--fs-sm);
    color: var(--text-2);
    padding: 0 0.5rem;
    white-space: nowrap;
    max-width: 22rem;
  }
  .status > span:last-child {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .status .spinner,
  .spinner.small {
    flex: none;
    width: 0.875rem;
    height: 0.875rem;
  }
  .ro,
  .warn {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-size: var(--fs-sm);
    font-weight: 600;
    padding: 0.25rem 0.5rem;
    border-radius: 1rem;
    background: var(--surface-3);
    color: var(--critical);
  }
  .warn {
    color: var(--negative);
  }
</style>
