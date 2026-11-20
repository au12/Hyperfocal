<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { api, urls, type Detection, type Subject } from '../lib/api';
  import { people, subjectLabel } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';
  import NameInput from './NameInput.svelte';
  import Popover from './Popover.svelte';

  let { id }: { id: number } = $props();

  const PAGE = 240;
  let subject = $state<Subject | null>(null);
  let tab = $state<'auto' | 'confirmed' | 'all'>('all');
  let dets = $state.raw<Detection[]>([]);
  let more = $state(false);
  let loading = $state(true);
  let renaming = $state(false);
  let moving = $state(false);
  const sel = new SvelteSet<number>();
  let anchor = -1;
  let ctl: AbortController | null = null;

  const named = $derived(!!subject?.name);
  const kind = $derived(subject?.kind ?? 'person');
  const isPet = $derived(kind !== 'person');
  const label = $derived(subject ? subjectLabel(subject) : '');
  const ro = $derived(!!store.status?.readonly);
  const detKind = $derived(kind === 'person' ? 'face' : kind);

  async function loadSubject() {
    try {
      const s = await api.subject(id);
      const first = subject?.id !== s.id;
      subject = s;
      if (first) tab = s.name ? (s.review > 0 ? 'auto' : 'confirmed') : 'all';
      else if (tab === 'auto' && s.review === 0) tab = 'confirmed';
    } catch {
      // Merged into another or removed.
      people.open = null;
    }
  }

  async function loadDets(append = false) {
    ctl?.abort();
    const c = new AbortController();
    ctl = c;
    loading = true;
    try {
      const page = await api.subjectDetections(id, { state: tab, limit: PAGE, offset: append ? dets.length : 0 }, c.signal);
      if (c.signal.aborted) return;
      dets = append ? [...dets, ...page] : page;
      more = page.length === PAGE;
      for (const s of [...sel]) if (!dets.some((d) => d.id === s)) sel.delete(s);
    } catch {
      /* aborted */
    } finally {
      if (ctl === c) loading = false;
    }
  }

  // A new subject (the next group), or a change to who is who.
  $effect(() => {
    void id;
    void people.allRev;
    untrack(async () => {
      await loadSubject();
      loadDets();
    });
  });

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !(e.target instanceof HTMLInputElement)) {
        if (sel.size) sel.clear();
        else close();
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  function close() {
    people.open = null;
  }

  function setTab(t: typeof tab) {
    if (tab === t) return;
    tab = t;
    sel.clear();
    loadDets();
  }

  /** The next unnamed group (after naming or skipping this one). */
  function nextGroup(): number | null {
    const g = people.groups;
    const i = g.findIndex((x) => x.id === id);
    const next = g.slice(i + 1).find((x) => x.id !== id) ?? g.find((x) => x.id !== id);
    return next?.id ?? null;
  }

  async function onName(p: { subject?: number; name: string }) {
    if (!subject) return;
    renaming = false;
    const wasNamed = !!subject.name;
    const next = wasNamed ? null : nextGroup();
    if (p.subject != null && p.subject !== subject.id) {
      // An existing person or pet: this is them.
      if (wasNamed && !confirm(`Merge ${subject.name} into ${p.name}? All of ${subject.name}'s photos become ${p.name}'s.`)) return;
      const r = await people.merge([subject.id], p.subject, p.name);
      if (r && !wasNamed && next != null) people.open = next;
      return;
    }
    const ok = await people.rename(subject, p.name);
    if (ok && !wasNamed && next != null) people.open = next;
  }

  function skip() {
    const n = nextGroup();
    if (n != null) people.open = n;
    else close();
  }

  function click(d: Detection, i: number, e: MouseEvent) {
    if (e.shiftKey && anchor >= 0) {
      const [a, b] = anchor < i ? [anchor, i] : [i, anchor];
      for (let k = a; k <= b; k++) sel.add(dets[k].id);
    } else if (sel.has(d.id)) sel.delete(d.id);
    else sel.add(d.id);
    anchor = i;
  }

  const selIds = () => [...sel];

  async function confirmIds(ids: number[]) {
    sel.clear();
    await people.confirm({ ids });
  }

  async function rejectIds(ids: number[]) {
    sel.clear();
    await people.reject(ids);
  }

  async function ignoreIds(ids: number[]) {
    sel.clear();
    await people.ignore(ids);
  }

  async function moveTo(p: { subject?: number; name: string }) {
    moving = false;
    const ids = selIds();
    sel.clear();
    await people.assign(ids, p.subject != null ? { subject: p.subject } : { name: p.name }, p.name);
  }

  function remove() {
    if (!subject) return;
    const what = subject.name ? `${subject.name}` : 'this group';
    if (confirm(`Remove ${what}? Its ${subject.photos} photo${subject.photos === 1 ? '' : 's'} stay; the faces become unknown again${subject.name ? ` and the name ${subject.name} is taken off the sidecars` : ''}.`)) {
      people.remove(subject);
    }
  }

  const fmt = (n: number) => n.toLocaleString();
  const when = (t: number | null | undefined) =>
    t ? new Date(t * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', timeZone: 'UTC' }) : '';
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="dialog sheet" role="dialog" aria-modal="true" aria-label={label}>
    <header>
      <span class="avatar" class:pet={isPet}>
        {#if subject?.cover}<img src={urls.crop(subject.cover)} alt="" />{:else}<Icon name={isPet ? 'paw' : 'user'} size={26} />{/if}
      </span>
      <div class="who grow">
        {#if subject && !ro && (!named || renaming)}
          <NameInput kind={detKind} value={subject.name ?? ''} exclude={subject.id} onpick={onName} oncancel={() => (named ? (renaming = false) : close())} placeholder={named ? 'New name' : isPet ? 'Name this pet' : 'Who is this?'} />
        {:else}
          <h2 class="ellipsis">
            {label}
            {#if named && !ro}<button class="btn sm ghost icon" onclick={() => (renaming = true)} title="Rename (or merge with someone of the same name)" aria-label="Rename"><Icon name="edit" /></button>{/if}
          </h2>
        {/if}
        {#if subject}
          <div class="sub dim tnum">
            {fmt(subject.photos)} {subject.photos === 1 ? 'photo' : 'photos'}{#if named && subject.review} · {fmt(subject.review)} to review{/if}{#if subject.hidden} · hidden{/if}
          </div>
        {/if}
      </div>
      <div class="acts">
        {#if subject && !named && !ro}
          <button class="btn sm" onclick={skip} title="The next group">Skip</button>
        {/if}
        {#if subject}
          <button class="btn sm" onclick={() => people.showPhotos(subject!)} title="Show these photos in the grid"><Icon name="grid" />{store.mobile ? '' : 'Photos'}</button>
          {#if !ro}
            <Popover align="right" width="15rem">
              {#snippet trigger(toggle, open)}
                <button class="btn sm icon" class:on={open} onclick={toggle} aria-label="More"><Icon name="more" /></button>
              {/snippet}
              {#snippet children(closeMenu)}
                {#if named}
                  <button class="menu-item" onclick={() => { closeMenu(); renaming = true; }}><Icon name="edit" />Rename or merge…</button>
                {/if}
                <button class="menu-item" onclick={() => { closeMenu(); people.setHidden(subject!, !subject!.hidden); }}>
                  <Icon name={subject!.hidden ? 'eye' : 'eye-off'} />{subject!.hidden ? 'Show again' : named ? 'Hide' : 'Hide (not of interest)'}
                </button>
                {#if named}
                  <button class="menu-item" onclick={() => { closeMenu(); people.removeName(subject!); }}><Icon name="tag" />Take the name off</button>
                {/if}
                <div class="menu-sep"></div>
                <button class="menu-item danger" onclick={() => { closeMenu(); remove(); }}><Icon name="x" />{named ? `Remove ${subject!.name}` : 'Remove this group'}</button>
              {/snippet}
            </Popover>
          {/if}
        {/if}
        <button class="btn ghost icon" onclick={close} aria-label="Close"><Icon name="x" /></button>
      </div>
    </header>

    {#if named}
      <div class="tabs">
        <div class="seg">
          <button class:on={tab === 'auto'} onclick={() => setTab('auto')} disabled={!subject?.review}>To review{#if subject?.review}<span class="n tnum">{fmt(subject.review)}</span>{/if}</button>
          <button class:on={tab === 'confirmed'} onclick={() => setTab('confirmed')}>Confirmed<span class="n tnum">{fmt(subject?.confirmed ?? 0)}</span></button>
        </div>
        {#if tab === 'auto' && subject?.review && !ro}
          <span class="dim q">Is this {subject.name}?</span>
          <button class="btn sm primary" onclick={() => people.confirm({ subject: id })}><Icon name="check" />Yes, all {fmt(subject.review)}</button>
        {/if}
      </div>
    {:else if subject && !ro}
      <div class="tabs"><span class="dim q">{isPet ? 'The same pet?' : 'The same person?'} Take out any that are not, then name the group.</span></div>
    {/if}

    <div class="body">
      <div class="faces" class:selecting={sel.size > 0}>
        {#each dets as d, i (d.id)}
          <div class="tile" class:sel={sel.has(d.id)} class:pet={isPet}>
            <button class="pick" onclick={(e) => click(d, i, e)} title={`${d.file ?? ''}${d.taken ? ' · ' + when(d.taken) : ''}${d.sim != null ? ` · match ${Math.round(d.sim * 100)}%` : ''}`}>
              <img src={urls.crop(d)} alt="" loading="lazy" draggable="false" />
              {#if d.q < 0.35 && d.kind === 'face'}<span class="poor" title="A poor view (small, blurred, turned away or masked)"></span>{/if}
              <span class="check"><Icon name="check" size={14} /></span>
            </button>
            {#if !ro}
              <div class="tools">
                {#if d.state === 0 && named}<button onclick={() => confirmIds([d.id])} title={`Yes, ${subject?.name}`} aria-label="Confirm"><Icon name="check" size={15} /></button>{/if}
                <button onclick={() => rejectIds([d.id])} title={named ? `Not ${subject?.name}` : 'Not in this group'} aria-label="Not this one"><Icon name="x" size={15} /></button>
                <button onclick={() => subject && people.openPhoto(subject, d)} title="Open the photo" aria-label="Open the photo"><Icon name="image" size={15} /></button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
      {#if loading && !dets.length}<div class="spinner center"></div>{/if}
      {#if !loading && !dets.length}<p class="dim center-text">{tab === 'auto' ? 'Nothing to review.' : 'None.'}</p>{/if}
      {#if more}<button class="btn sm ghost loadmore" onclick={() => loadDets(true)} disabled={loading}>Show more</button>{/if}
    </div>

    {#if sel.size > 0 && !ro}
      <footer class="selbar">
        <span class="tnum"><b>{sel.size}</b> selected</span>
        <div class="grow"></div>
        {#if moving}
          <div class="mv">
            <NameInput kind={detKind} compact exclude={id} onpick={moveTo} oncancel={() => (moving = false)} placeholder={isPet ? 'Which pet?' : 'Who is it?'} />
          </div>
        {:else}
          {#if tab === 'auto' && named}<button class="btn sm primary" onclick={() => confirmIds(selIds())}><Icon name="check" />Confirm</button>{/if}
          <button class="btn sm" onclick={() => rejectIds(selIds())}><Icon name="user-x" />{named ? `Not ${subject?.name}` : 'Not in this group'}</button>
          <button class="btn sm" onclick={() => (moving = true)}><Icon name="user-check" />{isPet ? 'Another pet…' : 'Someone else…'}</button>
          <button class="btn sm" onclick={() => ignoreIds(selIds())} title={isPet ? 'Not a pet, or not of interest' : 'Not a face, or not of interest'}><Icon name="eye-off" />Ignore</button>
          <button class="btn sm ghost" onclick={() => sel.clear()}>Clear</button>
        {/if}
      </footer>
    {/if}
  </div>
</div>

<style>
  .sheet {
    width: min(62rem, 100%);
    height: min(88vh, 54rem);
    max-height: none;
  }
  header {
    align-items: center;
  }
  .avatar {
    width: 3.5rem;
    height: 3.5rem;
    border-radius: 50%;
    background: var(--surface-2);
    overflow: hidden;
    display: grid;
    place-items: center;
    flex: none;
    color: var(--text-3);
  }
  .avatar.pet {
    border-radius: 0.875rem;
  }
  .avatar img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    max-width: 26rem;
  }
  .who h2 {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    margin: 0;
    font-size: var(--fs-xl);
  }
  .sub {
    font-size: var(--fs-sm);
  }
  .acts {
    display: flex;
    gap: 0.375rem;
    align-items: center;
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.625rem 1.25rem;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .tabs .seg > button {
    height: 1.875rem;
    font-size: var(--fs-sm);
    gap: 0.375rem;
  }
  .tabs .n {
    opacity: 0.75;
  }
  .q {
    font-size: var(--fs-sm);
  }
  .body {
    flex: 1;
    min-height: 0;
  }
  .faces {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(6.5rem, 1fr));
    gap: 0.5rem;
  }
  .tile {
    position: relative;
    aspect-ratio: 1;
    border-radius: var(--r-lg);
    overflow: hidden;
    background: var(--surface-2);
  }
  .pick {
    display: block;
    width: 100%;
    height: 100%;
    padding: 0;
    border: 0;
    background: none;
    cursor: pointer;
  }
  .pick img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .tile.sel {
    box-shadow: 0 0 0 3px var(--accent-strong);
  }
  .tile.sel img {
    opacity: 0.8;
  }
  .check {
    position: absolute;
    left: 0.375rem;
    top: 0.375rem;
    width: 1.25rem;
    height: 1.25rem;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.9);
    background: rgba(0, 0, 0, 0.25);
    color: transparent;
    display: grid;
    place-items: center;
    opacity: 0;
    transition: opacity 0.1s;
  }
  .tile:hover .check,
  .selecting .check {
    opacity: 1;
  }
  .tile.sel .check {
    background: var(--accent-strong);
    border-color: var(--accent-strong);
    color: #fff;
  }
  .poor {
    position: absolute;
    right: 0.375rem;
    top: 0.375rem;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--critical);
    box-shadow: 0 0 0 2px rgba(0, 0, 0, 0.35);
  }
  .tools {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    justify-content: center;
    gap: 0.25rem;
    padding: 0.25rem;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.55));
    opacity: 0;
    transition: opacity 0.1s;
  }
  .tile:hover .tools {
    opacity: 1;
  }
  .tools button {
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    border: 0;
    background: rgba(255, 255, 255, 0.92);
    color: #131e29;
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .tools button:hover {
    background: #fff;
  }
  .selbar {
    align-items: center;
    flex-wrap: wrap;
    justify-content: flex-start;
  }
  .mv {
    width: min(18rem, 100%);
  }
  .center {
    margin: 2rem auto;
  }
  .center-text {
    text-align: center;
    margin: 2rem 0;
  }
  .loadmore {
    display: block;
    margin: 1rem auto 0;
  }
  .danger,
  .danger :global(svg) {
    color: var(--negative);
  }
  @media (hover: none) {
    .tools {
      opacity: 1;
    }
    .check {
      opacity: 1;
    }
  }
  @media (max-width: 760px) {
    :global(.scrim):has(.sheet) {
      padding: 0;
    }
    .sheet {
      width: 100%;
      height: 100%;
      border-radius: 0;
      padding-top: env(safe-area-inset-top);
    }
    .faces {
      grid-template-columns: repeat(auto-fill, minmax(5.25rem, 1fr));
      gap: 0.375rem;
    }
    .tabs {
      padding: 0.5rem 0.75rem;
    }
    header {
      flex-wrap: wrap;
    }
  }
</style>
