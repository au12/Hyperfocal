<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { api, urls, type Detection, type Subject } from '../lib/api';
  import { notWord, people, subjectLabel } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import { rise, segmented } from '../lib/motion';
  import DetMenu from './DetMenu.svelte';
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
  let menu = $state<{ det: Detection; at: { x: number; y: number; w: number; h: number }; anchor: HTMLElement | null } | null>(null);
  const sel = new SvelteSet<number>();
  let anchor = -1;
  let ctl: AbortController | null = null;

  const named = $derived(!!subject?.name);
  const kind = $derived(subject?.kind ?? 'person');
  const isPet = $derived(kind !== 'person');
  const label = $derived(subject ? subjectLabel(subject) : '');
  const detKind = $derived(kind === 'person' ? 'face' : kind);
  const kindName = $derived(kind === 'person' ? 'Person' : kind === 'cat' ? 'Cat' : 'Dog');
  /** Where this group is among the unnamed ones (to go through them one after another). */
  const pos = $derived(named ? -1 : people.groups.findIndex((x) => x.id === id));
  const selFaces = $derived(dets.filter((d) => sel.has(d.id)).every((d) => d.kind === 'face'));
  /** Someone else with suggestions waiting, once this one has none left. */
  const nextReview = $derived(named && subject?.review === 0 ? (people.named.find((x) => x.review > 0 && x.id !== id) ?? null) : null);

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
      menu = null;
      await loadSubject();
      loadDets();
    });
  });

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !(e.target instanceof HTMLInputElement)) {
        if (menu) menu = null;
        else if (moving) moving = false;
        else if (sel.size) sel.clear();
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
    moving = false;
    loadDets();
  }

  /** The unnamed group after this one (or before it), to go on with. */
  function sibling(step: 1 | -1): number | null {
    const g = people.groups;
    if (!g.length) return null;
    const i = g.findIndex((x) => x.id === id);
    if (i < 0) return g[0].id !== id ? g[0].id : null;
    const n = g[(i + step + g.length) % g.length];
    return n && n.id !== id ? n.id : null;
  }

  function go(step: 1 | -1) {
    const n = sibling(step);
    if (n != null) people.open = n;
    else close();
  }

  async function onName(p: { subject?: number; name: string }) {
    if (!subject) return;
    renaming = false;
    const wasNamed = !!subject.name;
    const next = wasNamed ? null : sibling(1);
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

  function click(d: Detection, i: number, e: MouseEvent) {
    if (e.shiftKey && anchor >= 0) {
      const [a, b] = anchor < i ? [anchor, i] : [i, anchor];
      for (let k = a; k <= b; k++) sel.add(dets[k].id);
    } else if (sel.has(d.id)) sel.delete(d.id);
    else sel.add(d.id);
    anchor = i;
  }

  function openMenu(d: Detection, e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (menu?.det.id === d.id) {
      menu = null;
      return;
    }
    const btn = e.type === 'contextmenu' ? null : (e.currentTarget as HTMLElement);
    const r = btn?.getBoundingClientRect();
    menu = { det: d, at: r ? { x: r.left, y: r.top, w: r.width, h: r.height } : { x: e.clientX, y: e.clientY, w: 0, h: 0 }, anchor: btn };
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

  async function notIds(ids: number[]) {
    sel.clear();
    await people.notDetected(ids, detKind);
  }

  async function kindIds(ids: number[], k: 'dog' | 'cat') {
    sel.clear();
    await people.setKind(ids, k);
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
    <header class="head">
      <span class="avatar" class:pet={isPet}>
        {#if subject?.cover}<img src={urls.crop(subject.cover)} alt="" />{:else}<Icon name={isPet ? 'paw' : 'user'} size={26} />{/if}
      </span>
      <div class="who">
        {#if subject && (!named || renaming)}
          <div class="namefield">
            <NameInput kind={detKind} value={subject.name ?? ''} exclude={subject.id} listOnFocus={false} onpick={onName} oncancel={() => (named ? (renaming = false) : close())} placeholder={named ? 'New name' : isPet ? 'Name this pet' : 'Who is this?'} />
          </div>
        {:else}
          <h2>
            <span class="ellipsis">{label}</span>
            {#if named}<button class="btn sm ghost icon rename" onclick={() => (renaming = true)} title="Rename (or merge with someone of the same name)" aria-label="Rename"><Icon name="edit" /></button>{/if}
          </h2>
        {/if}
        {#if subject}
          <div class="sub tnum">
            {#if isPet}<span class="kchip"><Icon name={kind === 'cat' ? 'cat' : 'dog'} size={12} />{kindName}</span>{/if}
            <span>{fmt(subject.photos)} {subject.photos === 1 ? 'photo' : 'photos'}</span>
            {#if named && subject.review}<span class="dot">·</span><span class="rv">{fmt(subject.review)} to review</span>{/if}
            {#if !named}<span class="dot">·</span><span>not named yet</span>{/if}
            {#if subject.hidden}<span class="dot">·</span><span>hidden</span>{/if}
          </div>
        {/if}
      </div>
      <div class="acts">
        {#if subject && !named && people.groups.length > 1}
          <div class="nav">
            <button class="btn sm ghost icon" onclick={() => go(-1)} title="The group before" aria-label="Previous group"><Icon name="chevron-left" /></button>
            <span class="tnum">{pos >= 0 ? `${pos + 1} of ${people.groups.length}` : `${people.groups.length} groups`}</span>
            <button class="btn sm ghost icon" onclick={() => go(1)} title="Skip: the next group" aria-label="Next group"><Icon name="chevron" /></button>
          </div>
        {/if}
        {#if subject}
          <button class="btn sm" onclick={() => people.showPhotos(subject!)} title="Show these photos in the grid"><Icon name="grid" />{store.mobile ? '' : 'Show photos'}</button>
          <Popover align="right" width="16.5rem">
            {#snippet trigger(toggle, open)}
              <button class="btn sm icon" class:on={open} onclick={toggle} aria-label="More" title="More"><Icon name="more" /></button>
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
              {#if !named}
                <div class="menu-sep"></div>
                <div class="menu-label">Wrong detections?</div>
                {#if !isPet}
                  <button class="menu-item" onclick={() => { closeMenu(); people.setGroupKind(subject!, 'dog'); }}><Icon name="dog" />It’s a dog, not a person</button>
                  <button class="menu-item" onclick={() => { closeMenu(); people.setGroupKind(subject!, 'cat'); }}><Icon name="cat" />It’s a cat, not a person</button>
                {/if}
                <button class="menu-item warn" onclick={() => { closeMenu(); people.notGroup(subject!); }}>
                  <Icon name={isPet ? 'ban' : 'face-off'} />None of them {isPet ? 'are dogs or cats' : 'are faces'}
                </button>
              {/if}
              <div class="menu-sep"></div>
              <button class="menu-item danger" onclick={() => { closeMenu(); remove(); }}><Icon name="x" />{named ? `Remove ${subject!.name}` : 'Remove this group'}</button>
            {/snippet}
          </Popover>
        {/if}
        <button class="btn ghost icon" onclick={close} aria-label="Close" title="Close (Esc)"><Icon name="x" /></button>
      </div>
    </header>

    {#if named}
      <div class="subbar">
        <div class="seg" use:segmented>
          <button class:on={tab === 'auto'} onclick={() => setTab('auto')} disabled={!subject?.review}>To review{#if subject?.review}<span class="n tnum">{fmt(subject.review)}</span>{/if}</button>
          <button class:on={tab === 'confirmed'} onclick={() => setTab('confirmed')}>Confirmed<span class="n tnum">{fmt(subject?.confirmed ?? 0)}</span></button>
        </div>
        <div class="grow"></div>
        {#if tab === 'auto' && subject?.review}
          <span class="q">Are these all {subject.name}?</span>
          <button class="btn sm primary" onclick={() => people.confirm({ subject: id })}><Icon name="check" />Yes, all {fmt(subject.review)}</button>
        {:else if nextReview}
          <button class="btn sm ghost next" onclick={() => (people.open = nextReview.id)} title="Suggestions to review for someone else">
            Next to review: <b>{nextReview.name}</b><span class="n tnum">{fmt(nextReview.review)}</span><Icon name="chevron" />
          </button>
        {/if}
      </div>
    {:else if subject}
      <div class="subbar hint">
        <Icon name="info" size={15} />
        <span>{isPet ? 'All the same pet?' : 'All the same person?'} Take out any that are not{isPet ? '' : ' (or are not faces at all)'}, then name the group.</span>
      </div>
    {/if}

    <div class="body" class:selecting={sel.size > 0}>
      <div class="faces">
        {#each dets as d, i (d.id)}
          {@const s = sel.has(d.id)}
          <div class="tile" class:sel={s} class:pet={isPet} class:menu={menu?.det.id === d.id}>
            <button
              class="pick"
              onclick={(e) => click(d, i, e)}
              ondblclick={() => subject && people.openPhoto(subject, d)}
              oncontextmenu={(e) => openMenu(d, e)}
              aria-pressed={s}
              title={`${d.file ?? ''}${d.taken ? ' · ' + when(d.taken) : ''}${d.sim != null ? ` · ${Math.round(d.sim * 100)}% alike` : ''}\nClick to select · double-click to open the photo`}
            >
              <img src={urls.crop(d)} alt="" loading="lazy" draggable="false" />
            </button>
            <span class="check" aria-hidden="true"><Icon name="check" size={13} stroke={2.6} /></span>
            {#if tab === 'auto' && d.sim != null}<span class="sim tnum" title="How alike">{Math.round(d.sim * 100)}%</span>{/if}
            {#if d.q < 0.35 && d.kind === 'face'}<span class="poor" title="A poor view (small, blurred, turned away or masked): matched with more care"></span>{/if}
            <button class="tb more" onclick={(e) => openMenu(d, e)} title="More" aria-label="More" aria-haspopup="menu"><Icon name="more" size={15} stroke={2.2} /></button>
            {#if d.state === 0 && named}
              <div class="yn">
                <button class="tb ok" onclick={() => confirmIds([d.id])} title={`Yes, ${subject?.name}`} aria-label="Yes"><Icon name="check" size={15} stroke={2.4} /></button>
                <button class="tb no" onclick={() => rejectIds([d.id])} title={`Not ${subject?.name}`} aria-label="No"><Icon name="x" size={15} stroke={2.4} /></button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
      {#if loading && !dets.length}<div class="spinner center"></div>{/if}
      {#if !loading && !dets.length}
        <div class="none">
          <Icon name={tab === 'auto' ? 'check' : isPet ? 'paw' : 'users'} size={22} />
          <span>{tab === 'auto' ? 'Nothing left to review.' : 'Nothing here.'}</span>
        </div>
      {/if}
      {#if more}<button class="btn sm ghost loadmore" onclick={() => loadDets(true)} disabled={loading}>Show more</button>{/if}
    </div>

    {#if sel.size > 0}
      <div class="selbar glass" role="toolbar" aria-label="Selected faces" transition:rise={{ y: 18, scale: 0.96, duration: 280 }}>
        <span class="cnt tnum"><b>{sel.size}</b>{store.mobile ? '' : ' selected'}</span>
        {#if moving}
          <div class="mv">
            <NameInput kind={detKind} compact exclude={id} onpick={moveTo} oncancel={() => (moving = false)} placeholder={isPet ? 'Which pet is it?' : 'Who is it?'} />
          </div>
        {:else}
          {#if tab === 'auto' && named}
            <button class="btn sm primary" onclick={() => confirmIds(selIds())} title={`Yes, ${subject?.name}`}><Icon name="check" />{store.mobile ? '' : 'Confirm'}</button>
          {/if}
          <button class="btn sm" onclick={() => rejectIds(selIds())} title={named ? `Not ${subject?.name}` : 'Not in this group'}><Icon name="user-x" />{store.mobile ? '' : named ? `Not ${subject?.name}` : 'Not in this group'}</button>
          <button class="btn sm" onclick={() => (moving = true)} title={isPet ? 'Another pet' : 'Someone else'}><Icon name="users" />{store.mobile ? '' : isPet ? 'Another pet…' : 'Someone else…'}</button>
          <button class="btn sm wrong" onclick={() => notIds(selIds())} title={`The detector got ${sel.size === 1 ? 'it' : 'them'} wrong: ${notWord(selFaces ? 'face' : 'dog', sel.size)}`}>
            <Icon name={selFaces ? 'face-off' : 'ban'} />{store.mobile ? '' : notWord(selFaces ? 'face' : 'dog', sel.size).replace(/^n/, 'N')}
          </button>
          {#if selFaces || sel.size === 1}
            <Popover align="right" width="14.5rem" up>
              {#snippet trigger(toggle, open)}
                <button class="btn sm icon" class:on={open} onclick={toggle} title="More" aria-label="More"><Icon name="more" /></button>
              {/snippet}
              {#snippet children(closeMenu)}
                {#if selFaces}
                  <div class="menu-label">{sel.size === 1 ? 'Not a person?' : 'Not people?'}</div>
                  <button class="menu-item" onclick={() => { closeMenu(); kindIds(selIds(), 'dog'); }}><Icon name="dog" />{sel.size === 1 ? 'It’s a dog' : 'They’re dogs'}</button>
                  <button class="menu-item" onclick={() => { closeMenu(); kindIds(selIds(), 'cat'); }}><Icon name="cat" />{sel.size === 1 ? 'It’s a cat' : 'They’re cats'}</button>
                {/if}
                {#if sel.size === 1}
                  {#if selFaces}<div class="menu-sep"></div>{/if}
                  <button class="menu-item" onclick={() => { closeMenu(); const d = dets.find((x) => sel.has(x.id)); if (d && subject) people.openPhoto(subject, d); }}><Icon name="image" />Open the photo</button>
                {/if}
              {/snippet}
            </Popover>
          {/if}
          <button class="btn sm ghost icon" onclick={() => sel.clear()} title="Clear the selection" aria-label="Clear the selection"><Icon name="x" /></button>
        {/if}
      </div>
    {/if}
  </div>
</div>

{#if menu && subject}
  {@const m = menu}
  <DetMenu
    det={m.det}
    who={{ id: subject.id, name: subject.name }}
    at={m.at}
    anchor={m.anchor}
    showAll={false}
    onname={() => {
      sel.clear();
      sel.add(m.det.id);
      moving = true;
    }}
    onopen={() => subject && people.openPhoto(subject, m.det)}
    onclose={() => (menu = null)}
  />
{/if}

<style>
  .sheet {
    position: relative;
    width: min(64rem, 100%);
    height: min(88vh, 56rem);
    max-height: none;
  }
  .head {
    align-items: center;
    gap: 0.875rem;
    padding: 1rem 1rem 1rem 1.25rem;
  }
  .avatar {
    width: 3.75rem;
    height: 3.75rem;
    border-radius: 50%;
    background: var(--surface-2);
    overflow: hidden;
    display: grid;
    place-items: center;
    flex: none;
    color: var(--text-3);
    box-shadow:
      0 0 0 1px var(--border),
      0 8px 18px -10px rgba(0, 0, 0, 0.7);
  }
  .avatar.pet {
    border-radius: 1rem;
  }
  .avatar img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3125rem;
  }
  .who h2 {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    margin: 0;
    font-size: var(--fs-xl);
    font-weight: 650;
    min-width: 0;
  }
  .rename {
    opacity: 0.6;
  }
  .who h2:hover .rename {
    opacity: 1;
  }
  .namefield {
    width: min(22rem, 100%);
  }
  .sub {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.375rem;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .sub .rv {
    color: var(--accent);
    font-weight: 600;
  }
  .dot {
    opacity: 0.7;
  }
  .kchip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    height: 1.25rem;
    padding: 0 0.4375rem;
    border-radius: 999px;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    color: var(--text-2);
    font-weight: 600;
  }
  .acts {
    display: flex;
    gap: 0.375rem;
    align-items: center;
    flex: none;
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 0.125rem;
    margin-right: 0.375rem;
    padding: 0.125rem;
    border-radius: var(--r);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .nav span {
    padding: 0 0.25rem;
    min-width: 4rem;
    text-align: center;
  }
  .subbar {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.625rem 1.25rem;
    border-bottom: 1px solid var(--border);
    min-height: 3rem;
  }
  .subbar .seg > button {
    font-size: var(--fs-sm);
  }
  .subbar .n {
    color: var(--text-3);
    margin-left: 0.125rem;
  }
  .subbar .seg > button.on .n {
    color: var(--text-2);
  }
  .subbar.hint {
    font-size: var(--fs-sm);
    color: var(--text-2);
    background: var(--surface-2);
  }
  .subbar.hint :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .q {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .next {
    gap: 0.3125rem;
  }
  .next b {
    color: var(--text);
    font-weight: 600;
  }
  .body {
    flex: 1;
    min-height: 0;
    padding: 1rem 1.25rem 5.5rem;
  }
  .faces {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(7.25rem, 1fr));
    gap: 0.625rem;
  }
  .tile {
    position: relative;
    aspect-ratio: 1;
    border-radius: var(--r-lg);
    overflow: hidden;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    transition: box-shadow var(--dur-fast);
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
    transition:
      transform var(--dur-slow) var(--ease-out),
      opacity var(--dur);
  }
  .tile:hover .pick img {
    transform: scale(1.04);
  }
  .tile::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
    background: linear-gradient(180deg, rgba(0, 0, 0, 0.38), transparent 34%, transparent 60%, rgba(0, 0, 0, 0.5));
    opacity: 0;
    transition: opacity var(--dur);
  }
  .tile:hover::after,
  .tile.menu::after {
    opacity: 1;
  }
  .tile.sel {
    box-shadow:
      0 0 0 2.5px var(--accent-strong),
      0 0 0 5px color-mix(in srgb, var(--accent-strong) 25%, transparent);
  }
  .tile.sel .pick img {
    opacity: 0.82;
    transform: scale(0.94);
  }
  .tile.sel {
    background: color-mix(in srgb, var(--accent-strong) 30%, var(--surface-2));
  }
  .check {
    position: absolute;
    z-index: 1;
    left: 0.4375rem;
    top: 0.4375rem;
    width: 1.375rem;
    height: 1.375rem;
    border-radius: 50%;
    border: 1.5px solid rgba(255, 255, 255, 0.92);
    background: rgba(0, 0, 0, 0.22);
    color: transparent;
    display: grid;
    place-items: center;
    pointer-events: none;
    opacity: 0;
    transform: scale(0.85);
    transition:
      opacity var(--dur-fast),
      transform var(--dur) var(--ease-spring),
      background-color var(--dur-fast);
  }
  .tile:hover .check,
  .selecting .check {
    opacity: 1;
    transform: none;
  }
  .tile.sel .check {
    background: var(--accent-strong);
    border-color: var(--accent-strong);
    color: #fff;
  }
  .tb {
    width: 1.75rem;
    height: 1.75rem;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.92);
    color: #15171c;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.35);
    transition:
      background-color var(--dur-fast),
      color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out),
      opacity var(--dur-fast);
  }
  .tb:active {
    transform: scale(0.9);
  }
  .tb.more {
    position: absolute;
    z-index: 1;
    right: 0.375rem;
    top: 0.375rem;
    width: 1.625rem;
    height: 1.625rem;
    background: rgba(14, 15, 19, 0.62);
    color: #fff;
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    opacity: 0;
  }
  .tb.more:hover {
    background: rgba(14, 15, 19, 0.85);
  }
  .tile:hover .tb.more,
  .tile.menu .tb.more {
    opacity: 1;
  }
  .yn {
    position: absolute;
    z-index: 1;
    left: 0;
    right: 0;
    bottom: 0.4375rem;
    display: flex;
    justify-content: center;
    gap: 0.375rem;
    opacity: 0;
    transform: translateY(4px);
    transition:
      opacity var(--dur-fast),
      transform var(--dur) var(--ease-out);
  }
  .tile:hover .yn {
    opacity: 1;
    transform: none;
  }
  .tb.ok:hover {
    background: #2fb874;
    color: #fff;
  }
  .tb.no:hover {
    background: #e0524f;
    color: #fff;
  }
  .sim {
    position: absolute;
    z-index: 1;
    left: 0.4375rem;
    bottom: 0.4375rem;
    padding: 0.0625rem 0.375rem;
    border-radius: 999px;
    background: rgba(14, 15, 19, 0.66);
    color: #e9ebf2;
    font-size: 0.65625rem;
    font-weight: 650;
    pointer-events: none;
    transition: opacity var(--dur-fast);
  }
  .tile:hover .sim {
    opacity: 0;
  }
  .poor {
    position: absolute;
    z-index: 1;
    right: 0.5rem;
    bottom: 0.5rem;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--critical);
    box-shadow: 0 0 0 2px rgba(0, 0, 0, 0.4);
  }
  .tile:hover .poor {
    opacity: 0;
  }
  .selbar {
    position: absolute;
    z-index: 5;
    left: 50%;
    bottom: 1rem;
    translate: -50% 0;
    display: flex;
    align-items: center;
    gap: 0.375rem;
    padding: 0.3125rem 0.375rem 0.3125rem 0.9375rem;
    border-radius: 999px;
    box-shadow: var(--shadow-2);
    max-width: calc(100% - 1.5rem);
    white-space: nowrap;
  }
  .selbar .cnt {
    margin-right: 0.375rem;
    color: var(--text-2);
  }
  .selbar .cnt b {
    color: var(--text);
  }
  .mv {
    width: min(18rem, 60vw);
  }
  .selbar .wrong:hover {
    color: var(--negative);
    border-color: color-mix(in srgb, var(--negative) 40%, transparent);
    background: color-mix(in srgb, var(--negative) 10%, transparent);
  }
  .center {
    margin: 3rem auto;
  }
  .none {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    margin: 3.5rem 0;
    color: var(--text-3);
  }
  .loadmore {
    display: block;
    margin: 1.25rem auto 0;
  }
  .danger,
  .danger :global(svg),
  .warn,
  .warn :global(svg) {
    color: var(--negative);
  }
  @media (hover: none) {
    .check {
      opacity: 0.85;
      transform: none;
    }
    .tb.more {
      opacity: 1;
    }
    .yn {
      opacity: 1;
      transform: none;
    }
    .tile::after {
      opacity: 0.7;
    }
    .sim {
      bottom: auto;
      top: 0.4375rem;
      left: auto;
      right: 2.375rem;
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
    .head {
      flex-wrap: wrap;
      gap: 0.625rem;
      padding: 0.75rem 0.75rem 0.75rem 0.875rem;
    }
    .avatar {
      width: 3rem;
      height: 3rem;
    }
    .who h2 {
      font-size: var(--fs-lg);
    }
    .nav {
      margin-right: 0;
    }
    .nav span {
      min-width: 0;
    }
    .subbar {
      padding: 0.5rem 0.75rem;
      gap: 0.5rem;
    }
    .q {
      display: none;
    }
    .body {
      padding: 0.75rem 0.75rem calc(5rem + env(safe-area-inset-bottom));
    }
    .faces {
      grid-template-columns: repeat(auto-fill, minmax(6.25rem, 1fr));
      gap: 0.375rem;
    }
    .selbar {
      bottom: calc(0.75rem + env(safe-area-inset-bottom));
      padding-left: 0.75rem;
    }
  }
</style>
