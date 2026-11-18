<script lang="ts">
  import { onMount } from 'svelte';
  import { store, LABELS, fmtSettings, fmtSize, fmtDate, cameraName } from '../lib/store.svelte';
  import type { Patch } from '../lib/api';
  import { uiCtl } from '../lib/ctl';
  import Icon from './Icon.svelte';
  import EditField from './EditField.svelte';
  import KeywordEditor from './KeywordEditor.svelte';

  let { compact = false }: { compact?: boolean } = $props();

  let titleRef: HTMLInputElement | HTMLTextAreaElement | undefined = $state();
  let kwRef: HTMLInputElement | undefined = $state();

  onMount(() => {
    uiCtl.editTitle = () => requestAnimationFrame(() => titleRef?.focus());
    uiCtl.addKeyword = () => requestAnimationFrame(() => kwRef?.focus());
  });

  const multi = $derived(!store.loupe && store.selected.size > 1);
  const focusId = $derived(store.list.id[store.loupe ? store.current : store.cursor]);
  const d = $derived(store.detail && store.detail.id === focusId ? store.detail : null);
  const ro = $derived(!!store.status?.readonly);
  const ids = $derived(multi ? [...store.selected] : d ? [d.id] : []);

  function save(p: Patch) {
    if (!ids.length) return;
    store.patch(ids, p, ids.length > 1 ? `Updated ${ids.length} photos` : undefined);
  }

  function filterKeyword(k: string) {
    store.filters.kw = k;
    store.persist();
    store.loupe = false;
    store.search('');
  }

  const labelFor = (c: string | null) => LABELS.find((l) => l.color === c)?.name ?? c;
  const coord = (v: number | null, pos: string, neg: string) => (v == null ? '' : `${Math.abs(v).toFixed(5)}° ${v >= 0 ? pos : neg}`);
</script>

<div class="panel" class:compact>
  {#if multi}
    <div class="head">
      <div class="big">{store.selected.size} photos selected</div>
      <div class="dim">Changes apply to all of them and are written to their XMP sidecars.</div>
    </div>
    <section>
      <div class="lab">Rating</div>
      <div class="stars">
        {#each [1, 2, 3, 4, 5] as r}<button class="star" onclick={() => store.rate(r)}>★</button>{/each}
        <button class="btn sm ghost" onclick={() => store.rate(0)}>Clear</button>
      </div>
      <div class="lab">Label</div>
      <div class="row">
        {#each LABELS as l}<button class="lbl" onclick={() => store.label(l.color)} title={l.name}><span class="label-dot label-{l.color}"></span></button>{/each}
        <button class="btn sm ghost" onclick={() => save({ label: null })}>None</button>
      </div>
      <div class="row gap">
        <button class="btn sm" onclick={() => store.togglePick(1)}><Icon name="flag" /> Pick</button>
        <button class="btn sm" onclick={() => save({ pick: 0 })}>Unflag</button>
      </div>
    </section>
    <section>
      <div class="lab">Add keywords</div>
      <KeywordEditor keywords={[]} onadd={(k) => save({ keywords_add: [k] })} onremove={() => {}} readonly={ro} bind:inputRef={kwRef} />
    </section>
    <section>
      <button class="btn wide" onclick={() => store.openLocation()} disabled={ro}><Icon name="pin" /> Set location for {store.selected.size} photos…</button>
    </section>
    <section class="fields">
      <EditField label="Title" value={null} placeholder="Set title for all" onsave={(v) => save({ title: v })} readonly={ro} />
      <EditField label="Caption" value={null} placeholder="Set caption for all" multiline onsave={(v) => save({ description: v })} readonly={ro} />
      <EditField label="Creator" value={null} placeholder="Set creator for all" onsave={(v) => save({ creator: v })} readonly={ro} />
      <EditField label="Copyright" value={null} placeholder="Set copyright for all" onsave={(v) => save({ rights: v })} readonly={ro} />
    </section>
  {:else if !d}
    <div class="empty dim">{store.list.id.length ? 'Loading…' : 'No photo selected'}</div>
  {:else}
    <div class="head">
      <div class="big ellipsis" title={d.name}>{d.name}</div>
      <button class="link ellipsis" onclick={() => store.openFolder(d.folder_id)} title="Open folder">{d.folder || 'Library'}</button>
    </div>

    <section>
      <div class="rate-row">
        <div class="stars">
          {#each [1, 2, 3, 4, 5] as r}
            <button class="star" class:on={d.rating >= r} onclick={() => store.rate(r)} title={`${r}`} disabled={ro}>★</button>
          {/each}
        </div>
        <button class="flagbtn" class:on={d.pick === 1} onclick={() => store.togglePick(1)} title="Pick" disabled={ro}><Icon name="flag" fill={d.pick === 1} /></button>
      </div>
      <div class="row labels">
        {#each LABELS as l}
          <button class="lbl" class:on={d.lcolor === l.color} onclick={() => store.label(l.color)} title={l.name} disabled={ro}><span class="label-dot label-{l.color}"></span></button>
        {/each}
        {#if d.label}<span class="dim small">{labelFor(d.lcolor) ?? d.label}{#if d.lcolor && d.label !== labelFor(d.lcolor)} ({d.label}){/if}</span>{/if}
      </div>
      {#if d.rating < 0}<div class="warn small">Rating: rejected (−1)</div>{/if}
    </section>

    <section class="fields">
      <EditField label="Title" value={d.title} onsave={(v) => save({ title: v })} readonly={ro} bind:inputRef={titleRef} />
      <EditField label="Caption" value={d.description} multiline onsave={(v) => save({ description: v })} readonly={ro} />
    </section>

    <section>
      <div class="lab row">
        <span class="grow">Keywords</span>
        <button class="btn sm ghost" onclick={() => (store.dialog = 'keywords')} title="Rename, merge or re-parent keywords across the library"><Icon name="tags" /> Manage</button>
      </div>
      <KeywordEditor
        keywords={d.keywords}
        hierarchical={d.hkeywords}
        onadd={(k) => save({ keywords_add: [k] })}
        onremove={(k) => save({ keywords_remove: [k] })}
        onpick={filterKeyword}
        readonly={ro}
        bind:inputRef={kwRef}
      />
    </section>

    <section>
      <div class="lab row">
        <span class="grow">Location</span>
        {#if !ro}<button class="btn sm ghost" onclick={() => store.openLocation([d.id])}><Icon name="pin" /> {d.lat != null ? 'Change' : 'Set on map'}</button>{/if}
      </div>
      {#if d.lat != null && d.lon != null}
        <div class="kv tnum">
          <a href={`https://www.openstreetmap.org/?mlat=${d.lat}&mlon=${d.lon}#map=15/${d.lat}/${d.lon}`} target="_blank" rel="noreferrer noopener">
            {coord(d.lat, 'N', 'S')}, {coord(d.lon, 'E', 'W')}
          </a>
          {#if d.alt != null}<span class="dim"> · {Math.round(d.alt)} m</span>{/if}
        </div>
      {:else}
        <div class="dim small">No GPS position</div>
      {/if}
      <div class="grid2">
        <EditField label="Place" value={d.sublocation} onsave={(v) => save({ sublocation: v })} readonly={ro} />
        <EditField label="City" value={d.city} onsave={(v) => save({ city: v })} readonly={ro} />
        <EditField label="State / Province" value={d.state} onsave={(v) => save({ state: v })} readonly={ro} />
        <EditField label="Country" value={d.country} onsave={(v) => save({ country: v })} readonly={ro} />
      </div>
    </section>

    <section>
      <div class="lab">Capture</div>
      <dl>
        {#if d.taken_str}<dt>Date</dt><dd>{fmtDate(d.taken_str)}</dd>{/if}
        {#if d.model || d.make}<dt>Camera</dt><dd>{cameraName(d.make, d.model)}</dd>{/if}
        {#if d.lens}<dt>Lens</dt><dd>{d.lens}</dd>{/if}
        {#if d.exposure || d.fnum || d.iso}
          <dt>Exposure</dt>
          <dd class="tnum">{fmtSettings(d, false)}</dd>
        {/if}
        {#if d.focal}<dt>Focal length</dt><dd>{Math.round(d.focal)} mm</dd>{/if}
      </dl>
    </section>

    <section>
      <div class="lab">File</div>
      <dl>
        <dt>Size</dt><dd class="tnum">{d.width && d.height ? `${d.width} × ${d.height} · ` : ''}{fmtSize(d.size)}</dd>
        <dt>Sidecar</dt>
        <dd>{#if d.sidecar}<span title="XMP sidecar next to the photo">{d.sidecar}</span>{:else}<span class="dim">none yet (created on first edit)</span>{/if}</dd>
        {#if d.files.length > 1}<dt>Files</dt><dd class="files">{d.files.join(', ')}</dd>{/if}
        {#if d.orientation !== 1}<dt>Orientation</dt><dd>EXIF {d.orientation}</dd>{/if}
      </dl>
      {#if d.error}<div class="warn small">Metadata: {d.error}</div>{/if}
    </section>

    {#if !compact || d.creator || d.rights}
      <section class="fields">
        <EditField label="Creator" value={d.creator} onsave={(v) => save({ creator: v })} readonly={ro} />
        <EditField label="Copyright" value={d.rights} onsave={(v) => save({ rights: v })} readonly={ro} />
      </section>
    {/if}
  {/if}
</div>

<style>
  .panel {
    padding: 0.875rem 1rem 2rem;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    min-width: 0;
  }
  .big {
    font-weight: 700;
    font-size: 1rem;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    text-align: left;
    font-size: var(--fs-sm);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  section + section {
    border-top: 1px solid var(--border);
    padding-top: 0.875rem;
  }
  .lab {
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-weight: 600;
  }
  .rate-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .stars {
    display: flex;
    align-items: center;
  }
  .star {
    border: 0;
    background: none;
    color: var(--border-strong);
    font-size: 1.5rem;
    line-height: 1;
    cursor: pointer;
    padding: 0 0.125rem;
  }
  .star.on {
    color: var(--star);
  }
  .star:hover:not(:disabled) {
    color: #ffd97a;
  }
  .flagbtn {
    border: 1px solid var(--border-strong);
    background: none;
    color: var(--text-3);
    width: 2.25rem;
    height: 2.25rem;
    border-radius: var(--r);
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .flagbtn.on {
    color: var(--pick);
    border-color: var(--pick);
  }
  .labels {
    gap: 0.375rem;
  }
  .lbl {
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    background: none;
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .lbl.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px var(--accent);
  }
  .fields {
    gap: 0.625rem;
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
  }
  dl {
    display: grid;
    grid-template-columns: 6.5rem 1fr;
    gap: 0.375rem 0.75rem;
    margin: 0;
  }
  dt {
    color: var(--text-3);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .files {
    font-size: var(--fs-sm);
  }
  .kv a {
    color: var(--accent);
    text-decoration: none;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .warn {
    color: var(--critical);
  }
  .gap {
    gap: 0.5rem;
  }
  .wide {
    width: 100%;
  }
  .empty {
    padding: 2rem 0;
    text-align: center;
  }
</style>
