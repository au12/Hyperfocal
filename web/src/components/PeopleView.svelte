<script lang="ts">
  import { onMount } from 'svelte';
  import { urls, type Subject } from '../lib/api';
  import { people, subjectLabel } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import { mlProgress, mlState } from '../lib/ml';
  import { segmented } from '../lib/motion';
  import Icon from './Icon.svelte';
  import Popover from './Popover.svelte';
  import SubjectSheet from './SubjectSheet.svelte';

  onMount(() => {
    people.loadStatus();
    people.load();
  });

  const st = $derived(people.status);
  const s = $derived(st?.status);
  const total = $derived(s ? s.analyzed + s.pending : 0);
  const pct = $derived(total ? Math.min(100, (100 * (s?.analyzed ?? 0)) / total) : 0);
  const pets = $derived(people.tab === 'pet');
  const ml = $derived(store.status?.ml);
  // While the models download, the bar shows how far.
  const fetchPct = $derived(mlProgress(ml));
  const busy = $derived(s?.phase === 'analysing' || s?.phase === 'grouping' || s?.phase === 'preparing');

  let find = $state('');
  const q = $derived(find.trim().toLowerCase());
  const named = $derived(q ? people.named.filter((x) => (x.name ?? '').toLowerCase().includes(q)) : people.named);
  /** Suggestions waiting for a yes or no, people with the most first. */
  const toReview = $derived([...people.named].filter((x) => x.review > 0).sort((a, b) => b.review - a.review));
  const reviewTotal = $derived(toReview.reduce((n, x) => n + x.review, 0));

  const phaseText: Record<string, string> = {
    off: 'Recognition is off',
    preparing: 'Getting the AI models ready',
    offline: 'The AI engine is not running',
    analysing: 'Analysing',
    idle: 'Up to date',
    paused: 'Paused',
    grouping: 'Forming groups',
    error: 'Error',
  };

  const fmt = (n: number) => n.toLocaleString();
  const photos = (n: number) => `${fmt(n)} ${n === 1 ? 'photo' : 'photos'}`;

  function openSheet(x: Subject) {
    people.open = x.id;
  }
</script>

{#snippet card(x: Subject, kind: 'named' | 'group' | 'hidden')}
  <button class="pcard" class:hidden={kind === 'hidden'} onclick={() => openSheet(x)} title={kind === 'group' ? `Name this ${pets ? 'pet' : 'person'} · ${photos(x.photos)}` : `${subjectLabel(x)} · ${photos(x.photos)}`}>
    <span class="face" class:pet={pets}>
      {#if x.cover}<img src={urls.crop(x.cover)} alt="" loading="lazy" draggable="false" />{:else}<Icon name={pets ? 'paw' : 'user'} size={30} />{/if}
      {#if kind === 'named' && x.review > 0}<span class="badge" title={`${fmt(x.review)} to review`}>{x.review > 99 ? '99+' : x.review}</span>{/if}
    </span>
    {#if kind === 'group'}
      <span class="name add"><Icon name="plus" size={13} stroke={2.2} />Add a name</span>
    {:else}
      <span class="name ellipsis">{subjectLabel(x)}</span>
    {/if}
    <span class="cnt tnum">{photos(x.photos)}</span>
  </button>
{/snippet}

<div class="people">
  <div class="bar">
    <div class="seg tabs" use:segmented>
      <button class:on={!pets} onclick={() => people.setTab('person')}><Icon name="users" />People{#if st}<span class="n tnum">{fmt(st.people)}</span>{/if}</button>
      <button class:on={pets} onclick={() => people.setTab('pet')}><Icon name="paw" />Pets{#if st}<span class="n tnum">{fmt(st.pets)}</span>{/if}</button>
    </div>
    {#if people.named.length > 8}
      <label class="find">
        <Icon name="search" size={14} />
        <input class="field" bind:value={find} placeholder={pets ? 'Find a pet' : 'Find a person'} spellcheck="false" autocomplete="off" onkeydown={(e) => { if (e.key === 'Escape') { find = ''; (e.currentTarget as HTMLInputElement).blur(); e.stopPropagation(); } }} />
        {#if find}<button class="clear" onclick={() => (find = '')} aria-label="Clear"><Icon name="x" size={12} /></button>{/if}
      </label>
    {/if}
    <div class="grow"></div>
    {#if s}
      <div class="state" class:ok={s.phase === 'idle'} class:warn={s.phase === 'offline' || s.phase === 'error'} title={s.error ?? s.current ?? ''}>
        {#if busy}<span class="spinner"></span>{:else if s.phase === 'idle'}<Icon name="check" size={14} stroke={2.2} />{:else if s.phase === 'paused'}<Icon name="pause" size={13} />{:else if s.phase === 'offline' || s.phase === 'error'}<Icon name="cloud-off" size={14} />{/if}
        <span class="ellipsis">
          {#if s.phase === 'preparing' && ml}{mlState(ml)}{:else}{phaseText[s.phase] ?? s.phase}{/if}{#if s.phase === 'analysing' && total}<span class="tnum">{` · ${fmt(s.analyzed)} of ${fmt(total)}`}</span>{#if s.rate > 0 && !store.mobile}<span class="dim tnum">{` · ${Math.round(s.rate)}/min`}</span>{/if}{/if}{#if s.phase === 'grouping' && s.regroup != null}<span class="tnum">{` · ${Math.round(s.regroup * 100)}%`}</span>{/if}
        </span>
        {#if s.phase === 'analysing' && total}<span class="meter"><span style:width={`${pct}%`}></span></span>{/if}
        {#if s.phase === 'preparing' && fetchPct != null}<span class="meter"><span style:width={`${fetchPct * 100}%`}></span></span>{/if}
      </div>
      {#if s.enabled && (s.phase === 'analysing' || st?.paused)}
        <button class="btn sm ghost icon" onclick={() => people.pause(!st?.paused)} title={st?.paused ? 'Resume the analysis' : 'Pause the analysis'} aria-label={st?.paused ? 'Resume' : 'Pause'}>
          <Icon name={st?.paused ? 'play' : 'pause'} />
        </button>
      {/if}
    {/if}
    <Popover align="right" width="18rem">
      {#snippet trigger(toggle, open)}
        <button class="btn sm icon" class:on={open} onclick={toggle} aria-label="More" title="Options"><Icon name="more" /></button>
      {/snippet}
      {#snippet children(close)}
        <label class="menu-item"><input type="checkbox" bind:checked={people.showSingles} onchange={() => people.load()} />Show groups of one photo</label>
        <label class="menu-item"><input type="checkbox" bind:checked={people.showHidden} onchange={() => people.load()} />Show hidden {pets ? 'pets' : 'people'}</label>
        <div class="menu-sep"></div>
        {#if s?.enabled}
          <button class="menu-item" onclick={() => { close(); people.pause(!st?.paused); }}><Icon name={st?.paused ? 'play' : 'pause'} />{st?.paused ? 'Resume the analysis' : 'Pause the analysis'}</button>
        {/if}
        <button class="menu-item" onclick={() => { close(); people.regroup(); }} disabled={!st || st.faces + st.animals === 0}><Icon name="refresh" />Form the groups again</button>
        <div class="menu-sep"></div>
        {#if st}
          <div class="stats">
            <div><span class="tnum">{fmt(st.faces)}</span> faces</div>
            <div><span class="tnum">{fmt(st.animals)}</span> dogs and cats</div>
            <div><span class="tnum">{fmt(s?.analyzed ?? 0)}</span> photos analysed{#if s && s.failed} · <span class="tnum">{fmt(s.failed)}</span> unreadable{/if}</div>
          </div>
          {#if !st.xmp}<div class="menu-note">Names stay in Hyperfocal: no sidecars are written (read-only mode, or <code>HYPERFOCAL_PEOPLE_XMP=false</code>).</div>{/if}
        {/if}
      {/snippet}
    </Popover>
  </div>

  <div class="scroll">
    {#if st && !people.enabled}
      <div class="empty-state">
        <div class="art"><Icon name="scan-face" size={40} /></div>
        <h2>People & pets</h2>
        <p>Hyperfocal can find the faces, dogs and cats in your photos, group them by who they are, and write the names you give them to the XMP sidecars (as <code>People|Name</code> and <code>Pets|Name</code> keywords, face regions and IPTC “person shown”), where Lightroom, digiKam and Excire read them.</p>
        <p>It is turned off on this server. The models run inside Hyperfocal, on the CPU or an NVIDIA GPU: set <code>HYPERFOCAL_ML</code> and <code>HYPERFOCAL_ML_FACES</code> to <code>true</code> (the default) and restart the container. The models, about 0.7 GB, are downloaded into your app data folder the first time. Your photos never leave your server.</p>
      </div>
    {:else}
      <div class="content">
        {#if s && s.phase === 'offline' && s.error}
          <div class="notice warn"><Icon name="cloud-off" /><span>The AI engine is not running ({s.error}). Names and groups still work; new photos are analysed when it is back.</span></div>
        {:else if s && s.phase === 'preparing' && ml?.phase === 'downloading'}
          <div class="notice"><Icon name="download" /><span>The face and pet models are downloading into your app data folder, once (about 0.7 GB; with search by content, 1.7 GB in all). The analysis starts as soon as they are in.</span></div>
        {/if}

        {#if reviewTotal > 0 && !q}
          <div class="review">
            <span class="stack" aria-hidden="true">
              {#each toReview.slice(0, 3) as x (x.id)}<span class="mini" class:pet={pets}>{#if x.cover}<img src={urls.crop(x.cover)} alt="" draggable="false" />{/if}</span>{/each}
            </span>
            <div class="rv-text">
              <b class="tnum">{fmt(reviewTotal)} {reviewTotal === 1 ? 'suggestion' : 'suggestions'} to review</b>
              <span class="ellipsis">{toReview.slice(0, 3).map((x) => `${x.name} ${fmt(x.review)}`).join(' · ')}{toReview.length > 3 ? ` · ${toReview.length - 3} more` : ''}</span>
            </div>
            <button class="btn sm primary" onclick={() => openSheet(toReview[0])}>Review</button>
          </div>
        {/if}

        {#if named.length}
          <section>
            <h3>{pets ? 'Pets' : 'People'}<span class="count tnum">{fmt(named.length)}</span></h3>
            <div class="cards">
              {#each named as x (x.id)}{@render card(x, 'named')}{/each}
            </div>
          </section>
        {:else if q}
          <p class="none">No {pets ? 'pet' : 'one'} called “{find.trim()}”.</p>
        {/if}

        {#if people.groups.length && !q}
          <section>
            <div class="sec-head">
              <h3>{people.named.length ? 'Not named yet' : pets ? 'Pets found' : 'People found'}<span class="count tnum">{fmt(people.groupsTotal)}</span></h3>
              <p class="hint">{pets ? 'Each group is one pet' : 'Each group is one person'}, most photos first. Name them to find them again{st?.xmp ? ' and to write the names to the sidecars' : ''}.</p>
            </div>
            <div class="cards">
              {#each people.groups as x (x.id)}{@render card(x, 'group')}{/each}
            </div>
            {#if people.groupsTotal > people.groups.length && !people.showSingles}
              <button class="btn sm ghost more" onclick={() => { people.showSingles = true; people.load(); }}>Show the {fmt(people.groupsTotal - people.groups.length)} groups of a single photo</button>
            {/if}
          </section>
        {/if}

        {#if people.showHidden && people.hidden.length && !q}
          <section>
            <div class="sec-head">
              <h3>Hidden<span class="count tnum">{fmt(people.hidden.length)}</span></h3>
              <p class="hint">Left out of the lists. Their names stay on the photos.</p>
            </div>
            <div class="cards">
              {#each people.hidden as x (x.id)}{@render card(x, 'hidden')}{/each}
            </div>
          </section>
        {/if}

        {#if !people.loading && !people.named.length && !people.groups.length && st}
          <div class="empty-state small">
            <div class="art"><Icon name={pets ? 'paw' : 'users'} size={34} /></div>
            {#if s?.phase === 'analysing' || (s?.pending ?? 0) > 0}
              <h2>Still looking</h2>
              <p>No {pets ? 'pets' : 'people'} yet: the photos are still being analysed. Groups appear here as {pets ? 'the same pet turns' : 'the same face turns'} up in more than one photo.</p>
            {:else}
              <h2>No {pets ? 'pets' : 'people'} yet</h2>
              <p>No {pets ? 'dog or cat' : 'face'} was found in more than one photo{people.showSingles ? '' : ' yet'}.</p>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

{#if people.open != null}
  <SubjectSheet id={people.open} />
{/if}

<style>
  .people {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.625rem;
    padding: 0.5rem 0.75rem 0.5rem 1rem;
    border-bottom: 1px solid var(--border);
    min-height: var(--toolbar-h);
  }
  .tabs > button {
    gap: 0.375rem;
  }
  .tabs .n {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin-left: 0.125rem;
  }
  .tabs > button.on .n {
    color: var(--text-2);
  }
  .find {
    position: relative;
    display: flex;
    align-items: center;
    width: min(15rem, 30vw);
  }
  .find > :global(svg) {
    position: absolute;
    left: 0.5625rem;
    color: var(--text-3);
    pointer-events: none;
  }
  .find .field {
    width: 100%;
    height: 1.875rem;
    padding-left: 1.875rem;
    padding-right: 1.75rem;
    font-size: var(--fs-sm);
  }
  .find .clear {
    position: absolute;
    right: 0.375rem;
    width: 1.25rem;
    height: 1.25rem;
    border: 0;
    border-radius: 50%;
    background: var(--surface-3);
    color: var(--text-2);
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 0.4375rem;
    color: var(--text-2);
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .state.ok > :global(svg) {
    color: var(--positive);
  }
  .state.warn {
    color: var(--critical);
  }
  .state .spinner {
    width: 0.875rem;
    height: 0.875rem;
    flex: none;
  }
  .meter {
    width: 5rem;
    height: 0.25rem;
    border-radius: 1rem;
    background: var(--surface-3);
    overflow: hidden;
    flex: none;
  }
  .meter span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--spectrum);
    transition: width var(--dur-slow) var(--ease-out);
  }
  .scroll {
    flex: 1;
    overflow: auto;
  }
  .content {
    padding: 0.5rem 1.5rem 4rem;
    max-width: 96rem;
  }
  section {
    padding-top: 1.5rem;
  }
  section + section {
    margin-top: 1.25rem;
    border-top: 1px solid var(--border);
  }
  .sec-head {
    margin-bottom: 1rem;
  }
  h3 {
    font-size: var(--fs-lg);
    font-weight: 650;
    margin: 0 0 1rem;
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .sec-head h3 {
    margin-bottom: 0.25rem;
  }
  .count {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    border-radius: 999px;
    padding: 0.0625rem 0.5rem;
  }
  .hint {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .none {
    color: var(--text-3);
    margin: 2rem 0;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(8.75rem, 1fr));
    gap: 0.75rem 0.5rem;
  }
  .pcard {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.125rem;
    min-width: 0;
    padding: 0.875rem 0.5rem 0.75rem;
    border: 0;
    border-radius: var(--r-xl);
    background: none;
    color: var(--text);
    cursor: pointer;
    transition: background-color var(--dur);
  }
  .pcard:hover {
    background: var(--surface-2);
  }
  .pcard:focus-visible {
    outline-offset: 0;
  }
  .pcard.hidden .face {
    opacity: 0.55;
    filter: grayscale(0.5);
  }
  .face {
    position: relative;
    width: 6.5rem;
    height: 6.5rem;
    margin-bottom: 0.625rem;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--text-3);
    display: grid;
    place-items: center;
    box-shadow:
      0 0 0 1px var(--border),
      0 8px 20px -12px rgba(0, 0, 0, 0.7);
    transition:
      box-shadow var(--dur),
      transform var(--dur) var(--ease-out);
  }
  .face.pet {
    border-radius: 1.5rem;
  }
  .face img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: inherit;
  }
  .pcard:hover .face {
    transform: translateY(-2px);
    box-shadow:
      0 0 0 2px var(--surface-2),
      0 0 0 4px var(--accent),
      0 14px 26px -14px rgba(0, 0, 0, 0.8);
  }
  .face .badge {
    position: absolute;
    right: 0.125rem;
    top: 0.125rem;
    box-shadow: 0 0 0 2.5px var(--bg);
  }
  .pcard:hover .badge {
    box-shadow: 0 0 0 2.5px var(--surface-2);
  }
  .name {
    font-weight: 600;
    max-width: 100%;
    font-size: var(--fs);
  }
  .name.add {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    color: var(--accent);
    font-weight: 600;
  }
  .cnt {
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .more {
    margin-top: 1rem;
  }
  .review {
    display: flex;
    align-items: center;
    gap: 0.875rem;
    margin-top: 1rem;
    padding: 0.75rem 0.875rem;
    border-radius: var(--r-xl);
    background: linear-gradient(90deg, var(--accent-bg-strong), var(--accent-bg));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 28%, transparent);
  }
  .stack {
    display: flex;
    flex: none;
  }
  .mini {
    width: 2.25rem;
    height: 2.25rem;
    border-radius: 50%;
    overflow: hidden;
    background: var(--surface-3);
    box-shadow: 0 0 0 2px var(--surface);
  }
  .mini.pet {
    border-radius: 0.625rem;
  }
  .mini + .mini {
    margin-left: -0.625rem;
  }
  .mini img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .rv-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .rv-text b {
    font-size: var(--fs);
    color: var(--text);
    font-weight: 650;
  }
  .notice {
    display: flex;
    gap: 0.625rem;
    align-items: flex-start;
    padding: 0.75rem 1rem;
    margin-top: 1rem;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .notice :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .notice.warn :global(svg) {
    color: var(--critical);
  }
  .empty-state {
    max-width: 34rem;
    margin: 4rem auto;
    text-align: center;
    color: var(--text-2);
    line-height: 1.55;
  }
  .empty-state h2 {
    color: var(--text);
    font-size: var(--fs-xl);
    margin: 1rem 0 0.5rem;
  }
  .empty-state p {
    margin: 0.5rem 0;
  }
  .empty-state.small {
    margin-top: 5rem;
  }
  .art {
    width: 4.5rem;
    height: 4.5rem;
    border-radius: 50%;
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 25%, transparent);
    color: var(--accent);
    display: grid;
    place-items: center;
    margin: 0 auto;
  }
  code {
    font-family: var(--mono);
    font-size: 0.8125rem;
  }
  label.menu-item input {
    margin: 0;
    accent-color: var(--accent-strong);
  }
  .stats {
    padding: 0.375rem 0.625rem;
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .stats span {
    color: var(--text-2);
    font-weight: 600;
  }
  .menu-note {
    padding: 0.25rem 0.625rem 0.5rem;
    font-size: var(--fs-xs);
    color: var(--text-3);
    line-height: 1.5;
  }
  .menu-note code {
    font-size: 0.6875rem;
  }
  @media (max-width: 760px) {
    .bar {
      flex-wrap: wrap;
      padding: 0.5rem 0.625rem;
      row-gap: 0.5rem;
    }
    .find {
      order: 10;
      width: 100%;
    }
    .content {
      padding: 0.25rem 0.75rem 2.5rem;
    }
    section {
      padding-top: 1.125rem;
    }
    .cards {
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 0.25rem;
    }
    .pcard {
      padding: 0.625rem 0.25rem 0.5rem;
    }
    .face {
      width: min(5.75rem, 26vw);
      height: min(5.75rem, 26vw);
      margin-bottom: 0.5rem;
    }
    .face.pet {
      border-radius: 1.25rem;
    }
    .state .meter {
      display: none;
    }
    .review {
      gap: 0.625rem;
      padding: 0.625rem 0.75rem;
    }
    .mini {
      width: 1.875rem;
      height: 1.875rem;
    }
  }
</style>
