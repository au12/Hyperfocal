<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { store } from '../lib/store.svelte';
  import { api } from '../lib/api';
  import Icon from './Icon.svelte';

  interface KNode {
    path: string;
    key: string;
    name: string;
    segs: string[];
    count: number;
    children: KNode[];
  }

  let nodes = $state.raw<KNode[]>([]);
  let byKey = $state.raw(new Map<string, KNode>());
  let loading = $state(true);
  let error = $state('');
  let filter = $state('');
  const open = new SvelteSet<string>();
  let selKey = $state<string | null>(null);
  let renameText = $state('');
  let parentText = $state('');
  let mergeText = $state('');
  let jobId = $state<string | null>(null);
  /** Where the keyword being changed is going: selected once the tree reloads. */
  let follow: string[] | null = null;
  let busy = $state(false);
  let treeEl: HTMLDivElement | undefined = $state();

  const ro = $derived(!!store.status?.readonly);
  const sel = $derived(selKey ? (byKey.get(selKey) ?? null) : null);
  const job = $derived(jobId ? store.jobs[jobId] : undefined);
  const locked = $derived(busy || (!!job && !job.finished));

  const SEP = /\s*(?:\||›|>)\s*/;
  const parse = (s: string) => s.split(SEP).map((x) => x.trim()).filter(Boolean);
  const show = (segs: string[]) => segs.join(' › ');
  const lower = (s: string) => s.toLowerCase();
  const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? '' : 's'}`;

  async function load() {
    loading = true;
    error = '';
    try {
      const { nodes: list } = await api.keywordTree();
      const map = new Map<string, KNode>();
      const roots: KNode[] = [];
      for (const [path, count] of list) {
        const segs = path.split('|');
        const node: KNode = { path, key: lower(path), name: segs[segs.length - 1], segs, count, children: [] };
        map.set(node.key, node);
      }
      for (const n of map.values()) {
        const parent = n.segs.length > 1 ? map.get(lower(n.segs.slice(0, -1).join('|'))) : undefined;
        if (parent) parent.children.push(n);
        else roots.push(n);
      }
      const sort = (v: KNode[]) => {
        v.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base', numeric: true }));
        for (const n of v) sort(n.children);
      };
      sort(roots);
      byKey = map;
      nodes = roots;
      const dest = follow ? map.get(lower(follow.join('|'))) : undefined;
      follow = null;
      if (dest) {
        for (let i = 1; i < dest.segs.length; i++) open.add(lower(dest.segs.slice(0, i).join('|')));
        select(dest);
      } else if (selKey && !map.has(selKey)) selKey = null;
      else if (selKey) select(map.get(selKey)!);
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loading = false;
    }
  }
  onMount(load);

  // A change started here: reload the tree once it is done (or if it finished before
  // its progress could be seen).
  $effect(() => {
    if (!jobId) return;
    const j = store.jobs[jobId];
    if (j && !j.finished) return;
    jobId = null;
    untrack(load);
  });

  /** Visible rows: the expanded tree, or every match of the filter with its ancestors. */
  const rows = $derived.by(() => {
    const out: { n: KNode; depth: number }[] = [];
    const f = lower(filter.trim());
    if (f) {
      const keep = new Set<string>();
      for (const n of byKey.values()) {
        if (lower(n.name).includes(f)) {
          for (let i = 1; i <= n.segs.length; i++) keep.add(lower(n.segs.slice(0, i).join('|')));
        }
      }
      const walk = (list: KNode[], depth: number) => {
        for (const n of list) {
          if (!keep.has(n.key)) continue;
          out.push({ n, depth });
          walk(n.children, depth + 1);
        }
      };
      walk(nodes, 0);
      return out;
    }
    const walk = (list: KNode[], depth: number) => {
      for (const n of list) {
        out.push({ n, depth });
        if (open.has(n.key)) walk(n.children, depth + 1);
      }
    };
    walk(nodes, 0);
    return out;
  });

  const allPaths = $derived([...byKey.values()].map((n) => show(n.segs)));

  /** Other keywords with the same name: likely candidates for a merge. */
  const twins = $derived.by(() => {
    if (!sel) return [];
    const name = lower(sel.name);
    const out: KNode[] = [];
    for (const n of byKey.values()) if (n !== sel && lower(n.name) === name) out.push(n);
    return out.sort((a, b) => b.count - a.count).slice(0, 6);
  });

  function select(n: KNode) {
    selKey = n.key;
    renameText = n.name;
    parentText = show(n.segs.slice(0, -1));
    mergeText = '';
  }

  function toggle(n: KNode, e?: MouseEvent) {
    e?.stopPropagation();
    if (open.has(n.key)) open.delete(n.key);
    else open.add(n.key);
  }

  /** Show a keyword's node in the tree: open its ancestors, select it, scroll to it. */
  async function reveal(n: KNode) {
    filter = '';
    for (let i = 1; i < n.segs.length; i++) open.add(lower(n.segs.slice(0, i).join('|')));
    select(n);
    await tick();
    treeEl?.querySelector<HTMLElement>('.node.on')?.scrollIntoView({ block: 'nearest' });
  }

  function onTreeKey(e: KeyboardEvent) {
    if (!rows.length) return;
    const i = rows.findIndex((r) => r.n.key === selKey);
    const cur = i >= 0 ? rows[i].n : null;
    let next: KNode | null = null;
    if (e.key === 'ArrowDown') next = rows[Math.min(rows.length - 1, i + 1)].n;
    else if (e.key === 'ArrowUp') next = rows[Math.max(0, i - 1)].n;
    else if (e.key === 'ArrowRight' && cur?.children.length && !filter) {
      if (!open.has(cur.key)) open.add(cur.key);
      else next = cur.children[0];
    } else if (e.key === 'ArrowLeft' && cur && !filter) {
      if (open.has(cur.key)) open.delete(cur.key);
      else if (cur.segs.length > 1) next = byKey.get(lower(cur.segs.slice(0, -1).join('|'))) ?? null;
    } else return;
    e.preventDefault();
    if (next) {
      select(next);
      tick().then(() => {
        const el = treeEl?.querySelector<HTMLElement>('.node.on');
        el?.scrollIntoView({ block: 'nearest' });
        el?.focus();
      });
    }
  }

  async function move(to: string[] | null) {
    if (!sel || locked) return;
    busy = true;
    try {
      const r = await api.moveKeyword(sel.path, to ? to.join('|') : null);
      follow = to;
      store.onJob(r.job);
      jobId = r.job.id;
    } catch (e) {
      store.toast((e as Error).message, 'error', undefined, 7000);
    } finally {
      busy = false;
    }
  }

  // Each action's target, validated as the user types.
  const intoItself = (to: string[], from: string[]) => to.length > from.length && from.every((s, i) => lower(s) === lower(to[i]));
  const renameTo = $derived.by(() => {
    if (!sel) return null;
    const name = renameText.trim();
    if (!name || SEP.test(name) || name === sel.name) return null;
    return [...sel.segs.slice(0, -1), name];
  });
  const moveTo = $derived.by(() => {
    if (!sel) return null;
    const to = [...parse(parentText), sel.name];
    const same = to.length === sel.segs.length && to.every((s, i) => s === sel.segs[i]);
    return same || intoItself(to, sel.segs) ? null : to;
  });
  const mergeTo = $derived.by(() => {
    if (!sel) return null;
    const to = parse(mergeText);
    if (!to.length || lower(to.join('|')) === sel.key || intoItself(to, sel.segs)) return null;
    return to;
  });
  const mergeExists = $derived(!!mergeTo && byKey.has(lower(mergeTo.join('|'))));

  function remove() {
    if (!sel) return;
    const below = sel.children.length ? ' and every keyword below it' : '';
    if (confirm(`Remove “${show(sel.segs)}”${below} from ${plural(sel.count, 'photo')}? You can undo this from the History.`)) move(null);
  }

  function showPhotos() {
    if (!sel) return;
    store.dialog = null;
    store.showKeyword(sel.path);
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog km" role="dialog" aria-modal="true" aria-label="Keywords">
    <header>
      <Icon name="tags" />
      <h2>Keywords</h2>
      {#if byKey.size}<span class="dim small tnum">{plural(byKey.size, 'keyword')}</span>{/if}
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="cols">
      <div class="left">
        <div class="filter">
          <Icon name="search" size={16} />
          <input bind:value={filter} placeholder="Filter keywords" aria-label="Filter keywords" />
          {#if filter}<button class="clear" onclick={() => (filter = '')} aria-label="Clear filter"><Icon name="x" size={14} /></button>{/if}
        </div>
        <div class="tree" role="tree" aria-label="Keyword tree" tabindex="-1" bind:this={treeEl} onkeydown={onTreeKey}>
          {#if loading && !nodes.length}<div class="pad"><span class="spinner"></span></div>{/if}
          {#if error}<div class="pad err">Could not load the keywords: {error}</div>{/if}
          {#if !loading && !error && !nodes.length}<div class="pad dim">No keywords in the library yet.</div>{/if}
          {#if filter && !rows.length && nodes.length}<div class="pad dim">No keyword matches “{filter}”.</div>{/if}
          {#each rows as { n, depth } (n.key)}
            <div
              class="node"
              class:on={selKey === n.key}
              style:padding-left={`${0.25 + depth * 1.125}rem`}
              onclick={() => select(n)}
              ondblclick={() => n.children.length && !filter && toggle(n)}
              onkeydown={(e) => e.key === 'Enter' && select(n)}
              role="treeitem"
              aria-selected={selKey === n.key}
              aria-expanded={n.children.length ? open.has(n.key) || !!filter : undefined}
              tabindex={selKey === n.key || (!selKey && rows[0]?.n === n) ? 0 : -1}
            >
              {#if n.children.length && !filter}
                <button class="tw" onclick={(e) => toggle(n, e)} tabindex="-1" aria-label={open.has(n.key) ? 'Collapse' : 'Expand'}><Icon name={open.has(n.key) ? 'chevron-down' : 'chevron'} size={14} /></button>
              {:else}
                <span class="tw"></span>
              {/if}
              <span class="grow ellipsis" title={show(n.segs)}>{n.name}</span>
              <span class="count tnum">{n.count.toLocaleString()}</span>
            </div>
          {/each}
        </div>
      </div>
      <div class="right">
        {#if !sel}
          <div class="hint dim">
            <p>Pick a keyword to rename it, move it under another one, merge it into another or remove it.</p>
            <p>A change is written to the XMP sidecar of every photo that has the keyword, as one step you can undo from the History.</p>
          </div>
        {:else}
          <div class="selhead">
            <div class="path">{#each sel.segs as s, i}{#if i > 0}<span class="sep">›</span>{/if}{s}{/each}</div>
            <div class="dim small tnum">{plural(sel.count, 'photo')}{sel.children.length ? `, ${plural(sel.children.length, 'keyword')} below` : ''}</div>
            <button class="btn sm" onclick={showPhotos} title="Every photo with this keyword or one below it"><Icon name="search" />Show photos</button>
          </div>
          {#if twins.length}
            <div class="twins">
              <div class="lab">Same name elsewhere</div>
              {#each twins as t (t.key)}
                <div class="twin">
                  <button class="link grow ellipsis" onclick={() => reveal(t)} title="Show in the tree">{show(t.segs)}</button>
                  <span class="dim small tnum">{t.count.toLocaleString()}</span>
                  {#if !ro}<button class="btn sm ghost" onclick={() => (mergeText = show(t.segs))} disabled={locked}><Icon name="merge" />Merge into</button>{/if}
                </div>
              {/each}
            </div>
          {/if}
          {#if ro}
            <p class="dim small">Read-only mode: keywords cannot be changed.</p>
          {:else}
            <form class="act" onsubmit={(e) => { e.preventDefault(); if (renameTo) move(renameTo); }}>
              <label class="lab" for="km-rename">Rename</label>
              <div class="row">
                <input id="km-rename" class="field grow" bind:value={renameText} placeholder="New name" autocomplete="off" />
                <button class="btn" type="submit" disabled={!renameTo || locked}><Icon name="edit" />Rename</button>
              </div>
            </form>
            <form class="act" onsubmit={(e) => { e.preventDefault(); if (moveTo) move(moveTo); }}>
              <label class="lab" for="km-parent">Move under</label>
              <div class="row">
                <input id="km-parent" class="field grow" bind:value={parentText} list="km-paths" placeholder="Top level, or a parent such as Places › Portugal" autocomplete="off" />
                <button class="btn" type="submit" disabled={!moveTo || locked}><Icon name="reparent" />Move</button>
              </div>
              {#if moveTo}<div class="dim small">Becomes {show(moveTo)}</div>{/if}
            </form>
            <form class="act" onsubmit={(e) => { e.preventDefault(); if (mergeTo) move(mergeTo); }}>
              <label class="lab" for="km-merge">Merge into</label>
              <div class="row">
                <input id="km-merge" class="field grow" bind:value={mergeText} list="km-paths" placeholder="Another keyword" autocomplete="off" />
                <button class="btn" type="submit" disabled={!mergeTo || locked}><Icon name="merge" />Merge</button>
              </div>
              {#if mergeTo}<div class="dim small">{mergeExists ? `${sel.name} is folded into ${show(mergeTo)}` : `${show(mergeTo)} does not exist yet: the keyword is moved there`}</div>{/if}
            </form>
            <div class="act">
              <button class="btn danger" onclick={remove} disabled={locked}><Icon name="x" />Remove from {plural(sel.count, 'photo')}</button>
            </div>
          {/if}
        {/if}
        <datalist id="km-paths">
          {#each allPaths as p}<option value={p}></option>{/each}
        </datalist>
      </div>
    </div>
    {#if job}
      <footer class="progress">
        <div class="bar"><div style:width={`${(100 * job.done) / Math.max(1, job.total)}%`}></div></div>
        <span class="small tnum">{job.label}: {job.done.toLocaleString()} of {job.total.toLocaleString()}{job.failed ? ` · ${job.failed} failed` : ''}</span>
      </footer>
    {/if}
  </div>
</div>

<style>
  .dialog.km {
    width: min(56rem, 100%);
    height: min(86vh, 44rem);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .cols {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .left {
    width: 45%;
    min-width: 14rem;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
    color: var(--text-3);
  }
  .filter input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text);
    height: 1.75rem;
  }
  .clear {
    border: 0;
    background: none;
    color: var(--text-2);
    cursor: pointer;
    display: flex;
    padding: 0.25rem;
  }
  .tree {
    flex: 1;
    overflow: auto;
    padding: 0.25rem 0.375rem 1rem;
    outline: 0;
  }
  .pad {
    padding: 1rem;
  }
  .err {
    color: var(--negative);
  }
  .node {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    min-height: 1.875rem;
    padding-right: 0.5rem;
    border-radius: var(--r);
    cursor: pointer;
    user-select: none;
  }
  .node:hover {
    background: var(--surface-2);
  }
  .node:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .node.on {
    background: var(--accent-bg);
    color: var(--accent);
    font-weight: 600;
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
  .count {
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-weight: 400;
  }
  .right {
    flex: 1;
    min-width: 0;
    overflow: auto;
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .hint p {
    margin: 0 0 0.75rem;
  }
  .selhead {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    align-items: flex-start;
  }
  .path {
    font-weight: 700;
    font-size: 1rem;
    overflow-wrap: anywhere;
  }
  .sep {
    color: var(--text-3);
    margin: 0 0.375rem;
    font-weight: 400;
  }
  .twins {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  .twin {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-height: 1.875rem;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    text-align: left;
    color: var(--accent);
    cursor: pointer;
    font: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }
  .act {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    border-top: 1px solid var(--border);
    padding-top: 0.875rem;
  }
  .lab {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-3);
  }
  .act .btn.danger {
    align-self: flex-start;
  }
  .progress {
    flex-direction: column;
    align-items: stretch;
    gap: 0.375rem;
  }
  .bar {
    height: 0.375rem;
    border-radius: 0.25rem;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar > div {
    height: 100%;
    background: var(--accent-strong);
    transition: width 0.2s;
  }
  @media (max-width: 760px) {
    .dialog.km {
      height: 100%;
      max-height: 100%;
      border-radius: 0;
      padding-top: env(safe-area-inset-top);
    }
    :global(.scrim:has(.dialog.km)) {
      padding: 0;
    }
    .cols {
      flex-direction: column;
    }
    .left {
      width: 100%;
      height: 42%;
      flex: none;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .node {
      min-height: 2.5rem;
    }
  }
</style>
