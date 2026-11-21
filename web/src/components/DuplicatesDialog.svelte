<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { store, fmtSize } from '../lib/store.svelte';
  import { api, urls, type DupeGroup, type DupeMember, type DupeMode, type DupesStatus } from '../lib/api';
  import { dupes } from '../lib/dupes.svelte';
  import Icon from './Icon.svelte';

  let status = $state.raw<DupesStatus | null>(null);
  let acting = $state(false);
  let body = $state<HTMLDivElement>();
  let sentinel = $state<HTMLDivElement>();

  /** Mode, its name, a shorter one for phones, its icon. */
  const MODES = $derived<[DupeMode, string, string, string][]>([
    ['exact', 'Exact copies', 'Exact', 'copy'],
    ['near', 'Near-duplicates', 'Similar', 'layers'],
    ...(store.visualReady() || dupes.mode === 'visual' ? ([['visual', 'Look-alikes', 'Alike', 'sparkles']] as [DupeMode, string, string, string][]) : []),
    ['series', 'Series', 'Series', 'film'],
  ]);

  const r = $derived(dupes.report);
  const groups = $derived(dupes.groups);
  const readonly = $derived(!!store.status?.readonly);
  const folder = $derived(dupes.folder != null ? store.folders.get(dupes.folder) : undefined);
  const extra = $derived(groups.reduce((n, g) => n + g.members.length - 1, 0));
  const extraBytes = $derived(groups.reduce((n, g) => n + dupes.others(g).reduce((b, m) => b + m.bytes, 0), 0));
  const coveredPct = $derived(r && r.eligible ? Math.min(100, (100 * r.covered) / r.eligible) : 100);
  const hashing = $derived(!!status && ['hashing', 'waiting'].includes(status.phase));
  const partial = $derived(!!r && r.covered < r.eligible);

  async function refreshStatus() {
    status = await api.dupesStatus().catch(() => status);
  }

  onMount(() => {
    // The folder scopes cover the folder open in the grid, when one is.
    const root = store.roots[0]?.id;
    const fid = store.folderId;
    let reload = !dupes.report;
    if (!store.collection && !store.query && !store.dateRange && fid != null && fid !== root && fid !== dupes.folder) {
      dupes.folder = fid;
      if (dupes.scope !== 'library') reload = true;
    }
    if (dupes.scope !== 'library' && (dupes.folder == null || !store.folders.has(dupes.folder))) {
      dupes.scope = 'library';
      reload = true;
    }
    if (reload) dupes.load();
    else tick().then(() => body && (body.scrollTop = dupes.scroll));
    // Progress of the fingerprints while they are being made.
    const t = setInterval(() => {
      if (dupes.mode !== 'exact' && (hashing || partial)) refreshStatus();
    }, 3000);
    return () => {
      clearInterval(t);
      dupes.scroll = body?.scrollTop ?? 0;
    };
  });

  // New fingerprints arrived: the counts change, and a search now may find more.
  let rev = store.dupesRev;
  let fresh = $state(false);
  $effect(() => {
    if (store.dupesRev !== rev) {
      rev = store.dupesRev;
      if (dupes.mode !== 'exact' && !dupes.loading) fresh = true;
    }
    refreshStatus();
  });
  $effect(() => {
    void dupes.report;
    fresh = false;
  });

  // Draw more groups as the list scrolls.
  $effect(() => {
    const el = sentinel;
    if (!el || !body) return;
    const io = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting)) dupes.shown += 24;
    }, { root: body, rootMargin: '600px' });
    io.observe(el);
    return () => io.disconnect();
  });

  function setMode(m: DupeMode) {
    if (dupes.mode === m) return;
    dupes.mode = m;
    // A series stays in its folder: there is no "against the library" for them.
    if (m === 'series' && dupes.scope === 'against') dupes.scope = 'folder';
    dupes.load();
  }

  function setScope(e: Event) {
    dupes.scope = (e.target as HTMLSelectElement).value as typeof dupes.scope;
    dupes.load();
  }

  const plural = (n: number, one: string, many = one + 's') => `${n.toLocaleString()} ${n === 1 ? one : many}`;
  const mp = (m: DupeMember) => (m.w && m.h ? `${(m.w * m.h) / 1e6 >= 10 ? Math.round((m.w * m.h) / 1e6) : ((m.w * m.h) / 1e6).toFixed(1)} MP` : '');
  const day = (t: number | null) => (t ? new Date(t * 1000).toLocaleDateString(undefined, { timeZone: 'UTC', dateStyle: 'medium' }) : '');
  const time = (t: number | null) => (t ? new Date(t * 1000).toLocaleTimeString(undefined, { timeZone: 'UTC', timeStyle: 'medium' }) : '');
  const where = (m: DupeMember) => m.folder || 'Library';
  function alike(d: number | undefined): string {
    if (d == null) return '';
    if (d === 0) return 'Same picture';
    if (d <= 4) return 'Nearly the same';
    return 'Similar';
  }

  function title(g: DupeGroup): string {
    const n = g.members.length;
    if (dupes.mode === 'exact') return plural(n, 'exact copy', 'exact copies');
    if (dupes.mode === 'near') return plural(n, 'similar photo');
    if (dupes.mode === 'visual') return plural(n, 'look-alike');
    return plural(n, 'frame');
  }

  function subtitle(g: DupeGroup): string {
    const k = g.members.find((m) => m.id === dupes.keeperOf(g)) ?? g.members[0];
    const folders = new Set(g.members.map((m) => m.folder));
    const parts = [day(k.taken)];
    parts.push(folders.size === 1 ? where(k) : `in ${folders.size} folders`);
    const others = dupes.others(g).reduce((b, m) => b + m.bytes, 0);
    if (others) parts.push(`${fmtSize(others)} in the others`);
    return parts.filter(Boolean).join(' · ');
  }

  function choose(g: DupeGroup, id: number) {
    dupes.keep[g.key] = id;
  }

  /** The keeper first, then the rest as listed. */
  function ordered(g: DupeGroup): number[] {
    const k = dupes.keeperOf(g);
    return [k, ...g.members.filter((m) => m.id !== k).map((m) => m.id)];
  }

  async function compare(g: DupeGroup) {
    dupes.scroll = body?.scrollTop ?? 0;
    store.dialog = null;
    await store.showGroup(ordered(g), title(g));
    if (g.members.length > 4) store.toast(`Comparing 4 of ${g.members.length}: ↑/↓ steps the active pane through the rest`, 'info');
  }

  async function rejectOthers(g: DupeGroup) {
    const ids = dupes.others(g).map((m) => m.id);
    acting = true;
    try {
      await store.reject(ids);
    } finally {
      acting = false;
    }
  }

  /** Exact copies only: near-duplicates and series differ, so they go one group at a time. */
  async function rejectAll() {
    if (dupes.mode !== 'exact') return;
    const ids = groups.flatMap((g) => dupes.others(g).map((m) => m.id));
    if (!ids.length) return;
    const listed = r && r.total_groups > r.groups.length ? ' listed' : '';
    const where = `${plural(groups.length, 'group')}${listed}`;
    if (!confirm(`Move ${plural(ids.length, 'photo')} (${fmtSize(extraBytes)}) to ${store.status?.reject_dir ?? '_Rejected'}? Every photo not marked Keep goes, in the ${where}. History can undo it.`)) return;
    acting = true;
    try {
      await store.reject(ids);
    } finally {
      acting = false;
    }
  }

  async function dismiss(g: DupeGroup) {
    const ids = g.members.map((m) => m.id);
    try {
      await api.dupesDismiss(ids);
      dupes.dropGroup(g.key);
      store.toast(dupes.mode === 'series' ? 'Not shown as a series again' : 'Not shown as duplicates again', 'ok', {
        label: 'Undo',
        run: async () => {
          await api.dupesDismiss(ids, true);
          dupes.load();
        },
      });
    } catch (e) {
      store.toast((e as Error).message, 'error', undefined, 7000);
    }
  }

  async function showDismissed() {
    if (!r?.dismissed) return;
    try {
      await api.dupesClearDismissed();
      await dupes.load();
    } catch (e) {
      store.toast((e as Error).message, 'error', undefined, 7000);
    }
  }

  function openIntegrity() {
    store.dialog = 'integrity';
  }

  const headline = $derived.by(() => {
    if (!r) return '';
    const n = groups.length;
    if (!n) return dupes.mode === 'exact' ? 'No exact copies' : dupes.mode === 'near' ? 'No near-duplicates' : dupes.mode === 'visual' ? 'No look-alikes' : 'No series';
    const g = dupes.mode === 'series' ? plural(n, 'series', 'series') : plural(n, 'group');
    const x = dupes.mode === 'series' ? plural(extra, 'other frame') : plural(extra, dupes.mode === 'exact' ? 'extra copy' : 'other photo', dupes.mode === 'exact' ? 'extra copies' : 'other photos');
    return `${g} · ${x} · ${fmtSize(extraBytes)}`;
  });
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog dupes" role="dialog" aria-modal="true" aria-label="Duplicates">
    <header>
      <Icon name="copy" />
      <h2>Duplicates</h2>
      <button class="btn ghost icon" onclick={() => dupes.load()} disabled={dupes.loading} title="Search again" aria-label="Search again"><Icon name="refresh" /></button>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="controls">
      <div class="seg modes" role="tablist" aria-label="What to look for">
        {#each MODES as [m, label, short, icon] (m)}
          <button role="tab" aria-selected={dupes.mode === m} class:on={dupes.mode === m} onclick={() => setMode(m)} aria-label={label}><Icon name={icon} /><span class="long">{label}</span><span class="short">{short}</span></button>
        {/each}
      </div>
      <div class="row wrap opts">
        <select class="field scope" value={dupes.scope} onchange={setScope} aria-label="Where to look">
          <option value="library">Whole library</option>
          <option value="folder" disabled={!folder}>{folder ? `“${folder.name}” and its sub-folders` : 'A folder (open one first)'}</option>
          {#if dupes.mode !== 'series'}<option value="against" disabled={!folder}>{folder ? `“${folder.name}” against the rest of the library` : 'A folder against the library'}</option>{/if}
        </select>
        {#if dupes.mode === 'near' || dupes.mode === 'visual'}
          <div class="seg" role="radiogroup" aria-label="How alike">
            {#each [['strict', 'Strict'], ['normal', 'Normal'], ['loose', 'Loose']] as [v, label] (v)}
              <button role="radio" aria-checked={dupes.level === v} class:on={dupes.level === v} onclick={() => { dupes.level = v as typeof dupes.level; dupes.load(); }}>{label}</button>
            {/each}
          </div>
        {:else if dupes.mode === 'series'}
          <select class="field" value={String(dupes.gap)} onchange={(e) => { dupes.gap = +(e.target as HTMLSelectElement).value; dupes.load(); }} aria-label="Longest pause within a series">
            <option value="0">Bursts (same second)</option>
            <option value="2">Up to 2 s apart</option>
            <option value="5">Up to 5 s apart</option>
            <option value="15">Up to 15 s apart</option>
          </select>
        {/if}
      </div>
    </div>
    <div class="body" bind:this={body}>
      {#if dupes.error && !r}
        <p class="err">{dupes.error}</p>
      {:else if !r}
        <div class="center"><div class="spinner"></div></div>
      {:else}
        <div class="card" class:loading={dupes.loading}>
          <div class="big">
            <Icon name={groups.length ? 'copy' : 'check'} size={26} />
            <div class="grow">
              <div class="headline">{headline}</div>
              <div class="dim small">
                {#if dupes.mode === 'exact'}
                  Same image data, even when the metadata differs. The copy marked Keep stays; the others can go to {store.status?.reject_dir ?? '_Rejected'}.
                {:else if dupes.mode === 'near'}
                  Resized, recompressed, re-exported, turned or mirrored copies and light edits{dupes.level === 'loose' ? ', and frames that hardly differ' : ''}. Look before you reject: these are not identical files.
                {:else if dupes.mode === 'visual'}
                  Found by what the photos show, so crops, edits, other renderings and mirror images count too{dupes.level === 'loose' ? ', and the same scene from another angle' : ''}. Look before you reject: these are not identical files.
                {:else}
                  Photos one camera took in quick succession in one folder. Keep suggests the frame you rated or picked, else the sharpest.
                {/if}
              </div>
            </div>
          </div>
          {#if dupes.mode === 'exact' && !r.integrity}
            <div class="note">Exact copies come from the integrity monitor's hashes, and it is off (<code>HYPERFOCAL_INTEGRITY=false</code>). Near-duplicates find copies too.</div>
          {:else if partial}
            <div class="facts small">
              <div class="fact">
                <span class="dim">{dupes.mode === 'exact' ? 'Files hashed' : dupes.mode === 'visual' ? 'Photos looked at' : 'Photos fingerprinted'}</span>
                <span class="tnum">{r.covered.toLocaleString()} of {r.eligible.toLocaleString()} ({coveredPct < 99.95 ? coveredPct.toFixed(1) : '100'}%)</span>
              </div>
              <div class="bar"><div style:width={`${coveredPct}%`}></div></div>
              {#if dupes.mode === 'exact'}
                <div class="dim">The rest are hashed by the integrity monitor's nightly check. <button class="link" onclick={openIntegrity}>Archive integrity</button> can run it now.</div>
              {:else if dupes.mode === 'visual'}
                <div class="dim">Search by content looks at every photo once in the background, the newest first ({store.status?.visual?.phase === 'offline' ? 'the AI engine is not running' : store.status?.visual?.phase === 'preparing' ? 'its models are still downloading' : 'see the search field'}).</div>
              {:else if status && !status.enabled}
                <div class="dim">Fingerprinting is off (<code>HYPERFOCAL_DUPES=false</code>): only photos fingerprinted before are compared.</div>
              {:else if hashing}
                <div class="now ellipsis"><span class="spinner"></span>{status?.phase === 'waiting' ? 'Waiting for the thumbnails or the scan' : `Fingerprinting${status?.current ? `: ${status.current}` : ''}`}{status?.pending ? ` · ${status.pending.toLocaleString()} to go` : ''}</div>
              {:else}
                <div class="dim">Photos are fingerprinted in the background, a folder you search first.</div>
              {/if}
            </div>
          {/if}
          {#if fresh}<div class="dim small">More photos were fingerprinted since this search. <button class="link" onclick={() => dupes.load()}>Search again</button></div>{/if}
          {#if r.unreadable}<div class="dim small">{plural(r.unreadable, 'photo')} could not be read for a fingerprint.</div>{/if}
          {#if r.truncated}<div class="note">The search stopped at its limit of matches: loosen less, or search one folder at a time.</div>{/if}
          {#if r.total_groups > r.groups.length}<div class="dim small">Showing the newest {r.groups.length.toLocaleString()} of {r.total_groups.toLocaleString()} groups: reject or dismiss them and search again for the rest.</div>{/if}
          {#if groups.length || r.dismissed}
            <div class="row wrap">
              {#if groups.length && dupes.mode === 'exact'}
                <button class="btn danger" onclick={rejectAll} disabled={acting || readonly} title={readonly ? 'Read-only mode' : ''}><Icon name="reject" />Reject all but the keepers…</button>
              {:else if groups.length}
                <!-- Near-duplicates and series are not identical files: one group at a time, after a look. -->
                <span class="dim small">These are not identical files, so each group is rejected on its own, after a look.</span>
              {/if}
              {#if r.dismissed}<button class="btn ghost" onclick={showDismissed} disabled={dupes.loading}><Icon name="eye" />Show dismissed groups again</button>{/if}
            </div>
          {/if}
        </div>

        {#each groups.slice(0, dupes.shown) as g (g.key)}
          {@const k = dupes.keeperOf(g)}
          <section class="group">
            <div class="ghead">
              <div class="grow">
                <h3>{title(g)}</h3>
                <div class="dim small ellipsis">{subtitle(g)}</div>
              </div>
              <div class="acts">
                <button class="btn sm ghost" onclick={() => compare(g)} title="Side by side, the keeper first"><Icon name="compare" />Compare</button>
                <button class="btn sm ghost" onclick={() => dismiss(g)} title={dupes.mode === 'series' ? 'Never show these as a series again' : 'Never show these as duplicates again'}><Icon name="eye-off" />{dupes.mode === 'series' ? 'Not a series' : 'Not duplicates'}</button>
                <button class="btn sm" onclick={() => rejectOthers(g)} disabled={acting || readonly} title={readonly ? 'Read-only mode' : `Move the others to ${store.status?.reject_dir ?? '_Rejected'}`}>
                  <Icon name="reject" />Reject {g.members.length === 2 ? 'the other' : `the other ${g.members.length - 1}`}
                </button>
              </div>
            </div>
            <div class="strip">
              {#each g.members as m (m.id)}
                {@const keep = m.id === k}
                <button class="tile" class:keep onclick={() => choose(g, m.id)} aria-pressed={keep} title={keep ? 'Kept' : 'Keep this one instead'}>
                  <span class="thumb">
                    <img src={urls.thumb(m.id, m.v)} alt="" loading="lazy" decoding="async" />
                    {#if keep}<span class="tag"><Icon name="check" size={12} />Keep</span>{/if}
                    {#if m.pick === -1}<span class="flag" title="Flagged as rejected"><Icon name="x" size={12} /></span>{:else if m.pick === 1}<span class="flag pick" title="Picked"><Icon name="flag" size={12} /></span>{/if}
                  </span>
                  <span class="cap">
                    <span class="nm ellipsis" title={m.name}>{m.name}</span>
                    <span class="dim ellipsis" title={where(m)}>{#if r && dupes.scope === 'against'}<b class:in={m.inside}>{m.inside ? 'This folder' : 'Elsewhere'}</b> · {/if}{where(m)}</span>
                    <span class="dim facts1 tnum">{m.label.toUpperCase()}{mp(m) ? ` · ${mp(m)}` : ''} · {fmtSize(m.bytes)}</span>
                    <span class="dim facts1">
                      {#if m.rating > 0}<span class="stars" title="{m.rating} stars">{'★'.repeat(m.rating)}</span>{/if}
                      {#if m.lcolor}<span class="label-dot label-{m.lcolor}"></span>{/if}
                      {#if dupes.mode === 'series'}<span class="tnum">{time(m.taken)}</span>{:else if dupes.mode === 'visual' && !keep && m.alike != null}<span class="tnum">{m.alike}% alike</span>{:else if !keep && m.turned}<span class="turned">{m.turned}</span>{:else if !keep && m.d != null}<span>{alike(m.d)}</span>{/if}
                    </span>
                  </span>
                </button>
              {/each}
            </div>
            {#if k === g.keeper && g.reason}
              <div class="why dim small"><Icon name="check" size={12} />Keep: {g.reason}</div>
            {:else if k !== g.keeper}
              <div class="why dim small"><Icon name="check" size={12} />Keep: your choice</div>
            {/if}
          </section>
        {/each}
        {#if groups.length > dupes.shown}<div class="more" bind:this={sentinel}><div class="spinner"></div></div>{/if}

        <p class="foot dim small">
          Exact copies are files with the same image data, from the integrity monitor's hashes: for JPEG, HEIC, TIFF and DNG only the image counts, so a copy another app wrote metadata into still matches.
          Near-duplicates compare a 64-bit fingerprint of each photo's thumbnail, so they catch resized, recompressed and lightly edited copies, but not crops or rotations.
          A RAW and its JPEG twin count as one photo; photos in {store.status?.reject_dir ?? '_Rejected'} folders are left out.
          The keeper is RAW over JPEG, then the most pixels, then the most metadata; click a photo to keep it instead.
          Rejecting moves files to {store.status?.reject_dir ?? '_Rejected'}, like any reject; nothing is deleted, and History undoes it.
        </p>
      {/if}
    </div>
  </div>
</div>

<style>
  .dialog.dupes {
    width: min(64rem, 100%);
    height: min(90vh, 56rem);
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.625rem 1rem;
    padding: 0.75rem 1.25rem;
    border-bottom: 1px solid var(--border);
  }
  .opts {
    gap: 0.625rem;
  }
  .modes > button {
    white-space: nowrap;
  }
  .short {
    display: none;
  }
  .scope {
    max-width: 22rem;
  }
  .body {
    flex: 1;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 3rem 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .err {
    color: var(--negative);
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    padding: 1rem;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
    transition: opacity 0.15s;
  }
  .card.loading {
    opacity: 0.6;
  }
  .big {
    display: flex;
    gap: 0.875rem;
    align-items: flex-start;
  }
  .big > :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .headline {
    font-weight: 700;
    font-size: 1.0625rem;
  }
  .note {
    font-size: var(--fs-sm);
    padding: 0.5rem 0.75rem;
    border-radius: var(--r);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .facts {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .fact {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .bar {
    height: 0.3125rem;
    border-radius: 0.25rem;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar > div {
    height: 100%;
    background: var(--accent-strong);
  }
  .now {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--text-2);
  }
  .now .spinner {
    width: 0.875rem;
    height: 0.875rem;
    flex: none;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .wrap {
    flex-wrap: wrap;
  }
  .group {
    margin-top: 1.25rem;
    padding-bottom: 0.875rem;
    border-bottom: 1px solid var(--border);
  }
  .ghead {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.625rem;
  }
  .ghead h3 {
    margin: 0;
    font-size: var(--fs);
  }
  .acts {
    display: flex;
    gap: 0.125rem;
    flex: none;
  }
  .strip {
    display: flex;
    gap: 0.75rem;
    overflow-x: auto;
    padding: 0.125rem 0.125rem 0.375rem;
    scroll-snap-type: x proximity;
  }
  .tile {
    flex: none;
    width: 9.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.375rem;
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    scroll-snap-align: start;
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .tile:hover {
    border-color: var(--border-strong);
  }
  .tile.keep {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 1;
    border-radius: var(--r);
    overflow: hidden;
    background: var(--surface-3);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .tag {
    position: absolute;
    left: 0.375rem;
    top: 0.375rem;
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.125rem 0.4375rem 0.125rem 0.3125rem;
    border-radius: 1rem;
    background: var(--accent-strong);
    color: var(--on-accent, #fff);
    font-size: 0.6875rem;
    font-weight: 700;
  }
  .flag {
    position: absolute;
    right: 0.375rem;
    top: 0.375rem;
    display: grid;
    place-items: center;
    width: 1.25rem;
    height: 1.25rem;
    border-radius: 50%;
    background: var(--negative);
    color: #fff;
  }
  .flag.pick {
    background: var(--positive);
  }
  .cap {
    display: flex;
    flex-direction: column;
    gap: 0.0625rem;
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .nm {
    font-weight: 600;
  }
  .cap b {
    font-weight: 600;
    color: var(--text-2);
  }
  .cap b.in {
    color: var(--accent);
  }
  .facts1 {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    min-height: 1.1em;
    white-space: nowrap;
  }
  .stars {
    color: var(--star);
    letter-spacing: -0.05em;
  }
  .why {
    display: flex;
    align-items: center;
    gap: 0.3125rem;
    margin-top: 0.25rem;
  }
  .why :global(svg) {
    color: var(--accent);
  }
  .more {
    display: grid;
    place-items: center;
    padding: 1.5rem 0;
  }
  .foot {
    margin: 1.25rem 0 0;
    line-height: 1.5;
  }
  code {
    font-size: 0.85em;
  }
  @media (max-width: 760px) {
    .dialog.dupes {
      height: 100%;
      max-height: 100%;
      border-radius: 0;
      padding-top: env(safe-area-inset-top);
    }
    :global(.scrim:has(.dialog.dupes)) {
      padding: 0;
    }
    .controls {
      padding: 0.625rem 1rem;
    }
    .modes {
      width: 100%;
    }
    .modes > button {
      flex: 1;
      justify-content: center;
      padding: 0 0.5rem;
    }
    .long {
      display: none;
    }
    .short {
      display: inline;
    }
    .opts,
    .scope {
      width: 100%;
      max-width: none;
    }
    .ghead {
      flex-direction: column;
      align-items: stretch;
      gap: 0.5rem;
    }
    .acts {
      flex-wrap: wrap;
    }
    .tile {
      width: 8.25rem;
    }
  }
  .turned {
    color: var(--accent);
    font-weight: 600;
  }
</style>
