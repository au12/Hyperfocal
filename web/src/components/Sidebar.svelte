<script lang="ts">
  import { store } from '../lib/store.svelte';
  import type { Folder } from '../lib/api';
  import Icon from './Icon.svelte';

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

  function toggle(f: Folder, e: MouseEvent) {
    e.stopPropagation();
    if (store.expanded.has(f.id)) store.expanded.delete(f.id);
    else store.expanded.add(f.id);
  }

  function open(f: Folder) {
    store.openFolder(f.id);
    onpick?.();
  }

  function openAll() {
    if (!root) return;
    store.query = '';
    store.openFolder(root.id).then(() => {
      if (!store.recursive) {
        store.recursive = true;
        store.reload();
      }
    });
    onpick?.();
  }

  function quick(kind: 'picks' | 'fivestar' | 'nogps' | 'rejected') {
    if (!root) return;
    const f = store.filters;
    f.pick = kind === 'picks' ? 1 : null;
    f.rmin = kind === 'fivestar' ? 5 : 0;
    f.gps = kind === 'nogps' ? 0 : null;
    f.onlyRejected = kind === 'rejected';
    f.showRejected = kind === 'rejected';
    store.persist();
    store.query = '';
    store.folderId = root.id;
    store.recursive = true;
    store.view = 'grid';
    store.reload();
    onpick?.();
  }

  const fmt = (n: number) => (n >= 10000 ? `${(n / 1000).toFixed(0)}k` : n.toLocaleString());
</script>

<nav class="sidebar">
  <div class="section">
    <div class="sec-title">Library</div>
    <button class="item" class:active={store.folderId === root?.id && !store.query} onclick={openAll}>
      <Icon name="library" size={16} /><span class="grow ellipsis">All photos</span><span class="count">{root ? fmt(root.total) : ''}</span>
    </button>
    <button class="item" onclick={() => quick('picks')}><Icon name="flag" size={16} /><span class="grow">Picks</span></button>
    <button class="item" onclick={() => quick('fivestar')}><Icon name="star" size={16} /><span class="grow">Five stars</span></button>
    <button class="item" onclick={() => quick('nogps')}><Icon name="pin" size={16} /><span class="grow">Without location</span></button>
    <button class="item" onclick={() => quick('rejected')}><Icon name="folder-x" size={16} /><span class="grow">All rejected</span></button>
  </div>
  <div class="section folders">
    <div class="sec-title">Folders</div>
    {#each rows as f (f.id)}
      <div
        class="item"
        class:active={store.folderId === f.id && !store.query}
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
          <button class="tw" onclick={(e) => toggle(f, e)} aria-label="Expand">
            <Icon name={store.expanded.has(f.id) ? 'chevron-down' : 'chevron'} size={14} />
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
  </div>
</nav>

<style>
  .sidebar {
    width: var(--sidebar-w);
    flex: none;
    border-right: 1px solid var(--border);
    background: var(--surface);
    overflow-y: auto;
    overflow-x: hidden;
    padding: 0.5rem 0.375rem 2rem;
  }
  :global(.drawer) .sidebar {
    width: 100%;
    border: 0;
  }
  .section + .section {
    border-top: 1px solid var(--border);
    margin-top: 0.5rem;
    padding-top: 0.5rem;
  }
  .sec-title {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-3);
    padding: 0.375rem 0.625rem;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    min-height: 2rem;
    padding: 0 0.5rem;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    user-select: none;
  }
  .item :global(svg) {
    color: var(--text-3);
    flex: none;
  }
  .item:hover {
    background: var(--surface-2);
  }
  .item.active {
    background: var(--accent-bg);
    color: var(--accent);
    font-weight: 600;
  }
  .item.active :global(svg) {
    color: var(--accent);
  }
  .item.reject {
    color: var(--text-2);
    font-style: italic;
  }
  .item.reject :global(svg) {
    color: var(--reject);
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
    border-radius: 0.25rem;
  }
  .tw:hover {
    background: var(--surface-3);
  }
  .count {
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .empty {
    padding: 1rem;
    font-size: var(--fs-sm);
  }
</style>
