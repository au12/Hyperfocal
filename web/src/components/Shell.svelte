<script lang="ts">
  import { onMount } from 'svelte';
  import { store, applyTheme } from '../lib/store.svelte';
  import { uiCtl } from '../lib/ctl';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import { pop, fade } from '../lib/motion';

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
  const folderName = $derived(
    store.view === 'people'
      ? 'People & pets'
      : store.query
        ? 'Search'
        : store.collection?.id === 'dupes'
          ? 'Duplicates'
          : store.collection
          ? store.collection.name
          : store.dateRange
          ? store.dateRange.label
          : store.filters.subj?.length === 1 && store.subjLabels[store.filters.subj[0]]
          ? store.subjLabels[store.filters.subj[0]]
          : (store.folders.get(store.folderId ?? -1)?.name ?? 'Hyperfocal'),
  );
  const running = $derived(store.runningJobs());
  const problems = $derived(store.integrity.changed + store.integrity.missing + store.integrity.unreadable);
  const problemText = $derived(
    [store.integrity.changed && `${store.integrity.changed} changed`, store.integrity.missing && `${store.integrity.missing} missing`, store.integrity.unreadable && `${store.integrity.unreadable} unreadable`].filter(Boolean).join(', '),
  );
</script>

<header class="shell">
  {#if store.mobile}
    <button class="btn ghost icon" onclick={onmenu} aria-label="Folders"><Icon name="menu" /></button>
    {#if mobileSearch || store.query}
      <div class="search grow" in:pop={{ y: 0, scale: 0.98 }}>
        <Icon name="search" size={16} />
        <input bind:this={input} bind:value={q} oninput={onInput} placeholder="Search photos" enterkeyhint="search" />
        <button class="clear" onclick={clear} aria-label="Clear search"><Icon name="x" size={16} /></button>
      </div>
    {:else}
      {#if store.collection?.id === 'dupes'}
        <button class="title back grow ellipsis" onclick={() => (store.dialog = 'duplicates')} aria-label="Back to the Duplicates report"><Icon name="chevron-left" size={18} />{folderName}</button>
      {:else}
        <div class="title grow ellipsis">{folderName}</div>
      {/if}
      <button class="btn ghost icon" onclick={() => uiCtl.focusSearch()} aria-label="Search"><Icon name="search" /></button>
    {/if}
    {#if store.offline}<span class="ro" title="The server cannot be reached: showing the copies saved in this browser"><Icon name="cloud-off" size={14} /></span>{/if}
    {#if problems}<button class="btn ghost icon alarm" onclick={() => (store.dialog = 'integrity')} aria-label="Archive integrity: {problemText}"><Icon name="shield-alert" /><span class="dot">{problems > 99 ? '99+' : problems}</span></button>{/if}
    <button class="btn ghost icon" onclick={() => (store.dialog = 'settings')} aria-label="Settings"><Icon name="settings" /></button>
  {:else}
    <div class="brand">
      <img class="logo" src="./logo.png" alt="" width="28" height="28" />
      <span class="name">Hyperfocal</span>
    </div>
    <div class="search">
      <Icon name="search" size={15} />
      <input bind:this={input} bind:value={q} oninput={onInput} placeholder="Search file names, folders, titles, keywords, places, cameras…" />
      {#if q}<button class="clear" onclick={clear} aria-label="Clear search" in:pop><Icon name="x" size={15} /></button>{:else}<kbd>/</kbd>{/if}
    </div>
    <div class="right">
      {#if indexing}
        <div class="status" title={p?.current ?? ''} in:fade>
          <span class="spinner"></span>
          <span class="tnum">{p?.phase === 'walking' ? 'Reading folders…' : `Indexing ${p?.folders_done}/${p?.folders_total}`}</span>
        </div>
      {:else if p && p.thumbs_pending > 0}
        <div class="status" title="Building thumbnails in the background" in:fade>
          <span class="spinner"></span><span class="tnum">{p.thumbs_pending.toLocaleString()} thumbnails</span>
        </div>
      {/if}
      {#if p && !p.library_ok}
        <span class="warn" title={p.message ?? ''}><Icon name="info" size={15} /> Library offline</span>
      {/if}
      {#if store.status?.readonly}
        <span class="ro" title="The photo library is mounted read-only or HYPERFOCAL_READONLY is set"><Icon name="lock" size={13} /> Read-only</span>
      {/if}
      {#if store.offline}
        <span class="ro" title="The server cannot be reached: showing the copies saved in this browser"><Icon name="cloud-off" size={13} /> Offline</span>
      {/if}
      {#if running.length}
        {@const j = running[0]}
        <div class="status job" title={running.map((r) => r.label).join('\n')} in:fade>
          <span class="spinner"></span>
          <span class="tnum">{running.length > 1 ? `${running.length} changes running` : `${j.label}: ${j.done.toLocaleString()} of ${j.total.toLocaleString()}`}</span>
          {#if running.length === 1 && j.total > 0}<span class="progress"><i style:--p={j.done / j.total}></i></span>{/if}
        </div>
      {/if}
      {#if store.busy > 0}<span class="spinner small" title="Saving…"></span>{/if}
      {#if problems}
        <button class="btn ghost icon alarm" onclick={() => (store.dialog = 'integrity')} title="Archive integrity: {problemText}"><Icon name="shield-alert" /><span class="dot">{problems > 99 ? '99+' : problems}</span></button>
      {/if}
      <button class="btn ghost icon" onclick={() => (store.dialog = 'journal')} title={'History & undo' + store.keyHint('undo')}><Icon name="history" /></button>
      <Popover align="right">
        {#snippet trigger(toggle, open)}
          <button class="btn ghost icon" class:on={open} onclick={toggle} title="Library tools"><Icon name="wrench" /></button>
        {/snippet}
        {#snippet children(close)}
          <div class="menu-label">Library tools</div>
          <button class="menu-item" onclick={() => { close(); store.dialog = 'keywords'; }}><Icon name="tags" />Keywords</button>
          <button class="menu-item" onclick={() => { close(); store.dialog = 'geotag'; }}><Icon name="route" />Geotag from a track</button>
          <button class="menu-item" onclick={() => { close(); store.dialog = 'duplicates'; }}><Icon name="copy" />Duplicates</button>
          {#if store.status?.integrity !== false}<button class="menu-item" onclick={() => { close(); store.dialog = 'integrity'; }}><Icon name="shield" />Archive integrity</button>{/if}
        {/snippet}
      </Popover>
      <button class="btn ghost icon theme" onclick={toggleTheme} title="Light / dark">
        {#key store.settings.theme}<span class="swap" in:pop={{ y: 0, scale: 0.6, duration: 260 }}><Icon name={store.settings.theme === 'light' ? 'moon' : 'sun'} /></span>{/key}
      </button>
      <button class="btn ghost icon" onclick={() => (store.dialog = 'help')} title={'Keyboard shortcuts' + store.keyHint('help')}><Icon name="keyboard" /></button>
      <button class="btn ghost icon" onclick={() => (store.dialog = 'settings')} title="Settings"><Icon name="settings" /></button>
    </div>
  {/if}
  {#if indexing}<span class="scanline" aria-hidden="true"></span>{/if}
</header>

<style>
  .shell {
    height: var(--shell-h);
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0 0.625rem 0 0.875rem;
    padding-top: env(safe-area-inset-top);
    box-sizing: content-box;
    background: var(--shell);
    backdrop-filter: blur(20px) saturate(1.5);
    -webkit-backdrop-filter: blur(20px) saturate(1.5);
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
    user-select: none;
  }
  .logo {
    width: 1.75rem;
    height: 1.75rem;
    flex: none;
    border-radius: 0.4375rem;
    box-shadow:
      0 0 0 1px rgba(255, 255, 255, 0.08),
      0 4px 14px -4px rgba(120, 110, 255, 0.55);
    transition: transform var(--dur-slow) var(--ease-spring);
  }
  .brand:hover .logo {
    transform: rotate(-8deg) scale(1.06);
  }
  .name {
    font-weight: 650;
    font-size: 1rem;
    letter-spacing: -0.025em;
    background: linear-gradient(90deg, var(--text) 0%, var(--text) 45%, #ff9a3d 60%, #ff4f8b 70%, #a35bff 80%, #4f8cff 90%, var(--text) 100%);
    background-size: 250% 100%;
    background-position: 0 0;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    transition: background-position 0.9s var(--ease-out);
  }
  .brand:hover .name {
    background-position: 100% 0;
  }
  .title {
    font-weight: 650;
    font-size: 1rem;
    letter-spacing: -0.02em;
  }
  .title.back {
    display: flex;
    align-items: center;
    gap: 0.125rem;
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    text-align: left;
    cursor: pointer;
  }
  .search {
    flex: 1;
    max-width: 38rem;
    height: 2.125rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0 0.5rem 0 0.8125rem;
    border-radius: 999px;
    background: var(--field);
    box-shadow: inset 0 0 0 1px var(--field-border);
    color: var(--text-3);
    margin: 0 auto;
    transition:
      box-shadow var(--dur) var(--ease-out),
      background-color var(--dur-fast),
      max-width var(--dur-slow) var(--ease-out);
  }
  .search:hover {
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .search:focus-within {
    max-width: 42rem;
    color: var(--accent);
    background: var(--surface);
    box-shadow:
      inset 0 0 0 1px var(--accent),
      0 0 0 4px var(--accent-bg),
      0 10px 30px -12px rgba(99, 114, 255, 0.45);
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
  .search input:focus-visible {
    outline: none;
    animation: none;
  }
  .search input::placeholder {
    color: var(--text-3);
  }
  .search kbd {
    margin-right: 0.25rem;
  }
  .clear {
    border: 0;
    background: var(--surface-3);
    color: var(--text-2);
    cursor: pointer;
    display: flex;
    padding: 0.1875rem;
    border-radius: 50%;
    transition: background-color var(--dur-fast), color var(--dur-fast);
  }
  .clear:hover {
    color: var(--text);
    background: var(--border-strong);
  }
  .right {
    display: flex;
    align-items: center;
    gap: 0.125rem;
    flex: none;
  }
  .status {
    position: relative;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--fs-sm);
    color: var(--text-2);
    padding: 0.25rem 0.625rem;
    margin-right: 0.25rem;
    border-radius: 999px;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    white-space: nowrap;
    max-width: 22rem;
    overflow: hidden;
  }
  .status > span.tnum {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .status .progress {
    position: absolute;
    left: 0.625rem;
    right: 0.625rem;
    bottom: 2px;
    height: 2px;
    background: transparent;
  }
  .status .spinner,
  .spinner.small {
    flex: none;
    width: 0.8125rem;
    height: 0.8125rem;
  }
  .ro,
  .warn {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-size: var(--fs-sm);
    font-weight: 600;
    padding: 0.1875rem 0.5625rem;
    margin-right: 0.25rem;
    border-radius: 1rem;
    background: color-mix(in srgb, var(--critical) 14%, transparent);
    color: var(--critical);
  }
  .warn {
    color: var(--negative);
    background: color-mix(in srgb, var(--negative) 14%, transparent);
  }
  .alarm {
    position: relative;
    color: var(--negative);
  }
  .alarm .dot {
    position: absolute;
    top: 0.0625rem;
    right: 0;
    min-width: 1rem;
    height: 1rem;
    padding: 0 0.25rem;
    border-radius: 0.5rem;
    background: var(--negative);
    color: #fff;
    font-size: 0.625rem;
    font-weight: 700;
    line-height: 1rem;
    text-align: center;
    box-shadow: 0 0 0 2px var(--shell-solid);
    animation: alarm 2.4s var(--ease-in-out) infinite;
  }
  @keyframes alarm {
    0%,
    100% {
      transform: scale(1);
    }
    50% {
      transform: scale(1.12);
    }
  }
  .theme .swap {
    display: grid;
    place-items: center;
  }
  /* Indexing: a thin spectrum sweeps along the bottom edge of the bar. */
  .scanline {
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px;
    height: 2px;
    overflow: hidden;
    pointer-events: none;
  }
  .scanline::after {
    content: '';
    position: absolute;
    inset: 0;
    width: 35%;
    background: var(--spectrum);
    border-radius: 2px;
    animation: indeterminate 1.8s var(--ease-in-out) infinite;
  }
</style>
