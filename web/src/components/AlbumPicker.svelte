<script lang="ts">
  import { albums } from '../lib/albums.svelte';
  import { store } from '../lib/store.svelte';
  import { urls, type Album } from '../lib/api';
  import { rise } from '../lib/motion';
  import Icon from './Icon.svelte';

  // The "Add to album" dialog: find an album by typing, or create one with that name.
  // Arrow keys move, Enter adds; the album chosen becomes the target of B.

  const ids = albums.picking ?? [];
  let q = $state('');
  let at = $state(0);
  let list: HTMLElement | undefined = $state();

  const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();
  const matches = $derived.by((): Album[] => {
    const t = fold(q.trim());
    if (!t) {
      // The target album first, then the sidebar's order.
      const tgt = albums.byId(albums.target);
      return tgt ? [tgt, ...albums.list.filter((a) => a.id !== tgt.id)] : albums.list;
    }
    const hit = albums.list.filter((a) => fold(a.name).includes(t));
    return hit.sort((a, b) => Number(!fold(a.name).startsWith(t)) - Number(!fold(b.name).startsWith(t)));
  });
  const canCreate = $derived(q.trim().length > 0 && !albums.list.some((a) => fold(a.name) === fold(q.trim())));
  const total = $derived(matches.length + (canCreate ? 1 : 0));

  function setQuery(v: string) {
    q = v;
    at = 0;
  }

  function close() {
    albums.picking = null;
  }

  function choose(k: number) {
    if (k < 0 || k >= total) return;
    const a = matches[k];
    const name = q.trim();
    close();
    if (a) albums.add(a.id, ids);
    else if (name) albums.create(name, ids);
  }

  function move(d: number) {
    if (!total) return;
    at = (at + d + total) % total;
    requestAnimationFrame(() => list?.querySelector<HTMLElement>('.row.at')?.scrollIntoView({ block: 'nearest' }));
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') move(1);
    else if (e.key === 'ArrowUp') move(-1);
    else if (e.key === 'Enter') choose(at);
    else if (e.key === 'Escape') close();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  const fmt = (n: number) => n.toLocaleString();
</script>

<div class="scrim picker-scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="dialog picker" role="dialog" aria-modal="true" aria-label="Add to album">
    <header>
      <Icon name="album-plus" />
      <h2>Add {ids.length === 1 ? 'this photo' : `${fmt(ids.length)} photos`} to an album</h2>
      <button class="btn ghost icon" onclick={close} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="find">
      <Icon name="search" size={16} />
      <!-- svelte-ignore a11y_autofocus -->
      <input
        value={q}
        oninput={(e) => setQuery((e.target as HTMLInputElement).value)}
        onkeydown={onKey}
        placeholder={albums.list.length ? 'Find an album, or name a new one' : 'Name your first album'}
        maxlength="120"
        autofocus
        aria-label="Album"
      />
    </div>
    <div class="rows" bind:this={list} role="listbox" aria-label="Albums">
      {#each matches as a, k (a.id)}
        <button class="row" class:at={k === at} role="option" aria-selected={k === at} onclick={() => choose(k)} onpointermove={() => (at = k)} in:rise={{ y: 6, duration: 260, delay: Math.min(k, 8) * 22 }}>
          <span class="cover">
            {#if a.cover}<img src={urls.thumb(a.cover.id, a.cover.v)} alt="" loading="lazy" draggable="false" />{:else}<Icon name="album" size={18} />{/if}
          </span>
          <span class="grow">
            <span class="name ellipsis">{a.name}</span>
            <span class="sub dim tnum">{fmt(a.count)} {a.count === 1 ? 'photo' : 'photos'}{#if a.note} · {a.note}{/if}</span>
          </span>
          {#if albums.target === a.id}<span class="key" title="B adds photos to this album">B</span>{/if}
          <Icon name="arrow-up-right" size={15} class="go" />
        </button>
      {/each}
      {#if canCreate}
        <button class="row create" class:at={at === matches.length} role="option" aria-selected={at === matches.length} onclick={() => choose(matches.length)} onpointermove={() => (at = matches.length)} in:rise={{ y: 6, duration: 260 }}>
          <span class="cover plus"><Icon name="plus" size={18} /></span>
          <span class="grow"><span class="name ellipsis">Create “{q.trim()}”</span><span class="sub dim">A new album with {ids.length === 1 ? 'this photo' : `these ${fmt(ids.length)} photos`}</span></span>
        </button>
      {/if}
      {#if !total}
        <div class="empty dim">{albums.loaded ? 'Type a name to create your first album.' : 'Loading albums…'}</div>
      {/if}
    </div>
    <footer class="hint dim">
      <span><kbd>↑</kbd><kbd>↓</kbd> choose</span><span><kbd>Enter</kbd> add</span><span><kbd>{store.keyHint('albumAdd').replace(/[()\s]/g, '') || 'B'}</kbd> adds to the last album used</span>
    </footer>
  </div>
</div>

<style>
  .picker {
    width: min(27rem, 100%);
    max-height: min(36rem, 86vh);
  }
  .find {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    margin: 0.75rem 0.875rem 0.375rem;
    padding: 0 0.75rem;
    height: 2.5rem;
    border-radius: var(--r-lg);
    background: var(--field);
    box-shadow: inset 0 0 0 1px var(--field-border);
    color: var(--text-3);
    transition: box-shadow var(--dur);
  }
  .find:focus-within {
    box-shadow:
      inset 0 0 0 1px var(--accent),
      0 0 0 3px var(--accent-bg);
  }
  .find input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    outline: none;
    color: var(--text);
    font-size: var(--fs-lg);
  }
  .rows {
    overflow: auto;
    padding: 0.25rem 0.5rem 0.5rem;
    min-height: 3rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    width: 100%;
    padding: 0.4375rem 0.5rem;
    border: 0;
    border-radius: var(--r-lg);
    background: none;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: background-color var(--dur-fast);
  }
  .row.at {
    background: var(--accent-bg);
  }
  .row :global(.go) {
    color: var(--accent);
    opacity: 0;
    transform: translate(-4px, 4px);
    transition:
      opacity var(--dur-fast),
      transform var(--dur) var(--ease-out);
  }
  .row.at :global(.go) {
    opacity: 1;
    transform: none;
  }
  .cover {
    width: 2.75rem;
    height: 2.75rem;
    flex: none;
    border-radius: var(--r);
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    color: var(--text-3);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .cover.plus {
    background: var(--accent-bg-strong);
    color: var(--accent);
  }
  .grow {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 0.0625rem;
  }
  .name {
    font-weight: 600;
  }
  .sub {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .create .name {
    color: var(--accent);
  }
  .key {
    font-size: var(--fs-xs);
    font-weight: 700;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: 0.3125rem;
    padding: 0 0.3125rem;
    line-height: 1.125rem;
  }
  .empty {
    padding: 1.25rem 0.75rem;
    text-align: center;
  }
  .hint {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    justify-content: flex-start;
    font-size: var(--fs-xs);
    padding: 0.625rem 1.125rem 0.75rem;
  }
  kbd {
    font-family: var(--font);
    font-size: 0.625rem;
    font-weight: 650;
    padding: 0.0625rem 0.3125rem;
    margin-right: 0.25rem;
    border-radius: 0.25rem;
    background: var(--surface-3);
    color: var(--text-2);
    box-shadow: inset 0 -1px 0 var(--border-strong);
  }
  /* On a phone: a sheet from the bottom. */
  @media (max-width: 760px) {
    .picker-scrim {
      place-items: end stretch;
      padding: 0;
    }
    .picker {
      width: 100%;
      max-height: 80vh;
      border-radius: 1.125rem 1.125rem 0 0;
      padding-bottom: env(safe-area-inset-bottom);
      animation: sheet-up 0.34s var(--ease-out);
    }
    .hint {
      display: none;
    }
  }
  @keyframes sheet-up {
    from {
      transform: translateY(100%);
    }
  }
</style>
