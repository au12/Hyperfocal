<script lang="ts">
  import { onMount } from 'svelte';
  import { urls, type Subject } from '../lib/api';
  import { people, subjectLabel } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
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
  const ro = $derived(!!store.status?.readonly);

  const phaseText: Record<string, string> = {
    off: 'Face and pet recognition is off',
    connecting: 'Connecting to the ML service…',
    offline: 'The ML service is not reachable',
    analysing: 'Looking for faces and pets',
    idle: 'Up to date',
    paused: 'Paused',
    grouping: 'Forming groups',
    error: 'Error',
  };

  const fmt = (n: number) => n.toLocaleString();

  function openSheet(x: Subject) {
    people.open = x.id;
  }
</script>

<div class="people">
  <div class="bar">
    <div class="seg tabs">
      <button class:on={!pets} onclick={() => people.setTab('person')}><Icon name="users" />People{#if st}<span class="n tnum">{fmt(st.people)}</span>{/if}</button>
      <button class:on={pets} onclick={() => people.setTab('pet')}><Icon name="paw" />Pets{#if st}<span class="n tnum">{fmt(st.pets)}</span>{/if}</button>
    </div>
    {#if s}
      <div class="state" title={s.error ?? s.current ?? ''}>
        {#if s.phase === 'analysing' || s.phase === 'grouping'}<span class="spinner"></span>{/if}
        <span class="ellipsis">
          {phaseText[s.phase] ?? s.phase}{#if s.phase === 'analysing' && total}{` · ${fmt(s.analyzed)} of ${fmt(total)} photos`}{#if s.rate > 0}{` · ${Math.round(s.rate)}/min`}{/if}{/if}{#if s.phase === 'grouping' && s.regroup != null}{` · ${Math.round(s.regroup * 100)}%`}{/if}
        </span>
        {#if s.phase === 'analysing' && total}<span class="meter"><span style:width={`${pct}%`}></span></span>{/if}
      </div>
    {/if}
    <div class="grow"></div>
    {#if s?.enabled}
      <button class="btn sm" onclick={() => people.pause(!st?.paused)} title={st?.paused ? 'Resume the analysis' : 'Pause the analysis'}>
        <Icon name={st?.paused ? 'play' : 'pause'} />{store.mobile ? '' : st?.paused ? 'Resume' : 'Pause'}
      </button>
    {/if}
    <Popover align="right" width="17rem">
      {#snippet trigger(toggle, open)}
        <button class="btn sm icon" class:on={open} onclick={toggle} aria-label="More"><Icon name="more" /></button>
      {/snippet}
      {#snippet children(close)}
        <label class="menu-item"><input type="checkbox" bind:checked={people.showSingles} onchange={() => people.load()} />Show groups of one photo</label>
        <label class="menu-item"><input type="checkbox" bind:checked={people.showHidden} onchange={() => people.load()} />Show hidden {pets ? 'pets' : 'people'}</label>
        <div class="menu-sep"></div>
        <button class="menu-item" onclick={() => { close(); people.regroup(); }} disabled={!st || st.faces + st.animals === 0}><Icon name="refresh" />Form the groups again</button>
        <div class="menu-sep"></div>
        <div class="menu-label">
          {#if st}{fmt(st.faces)} faces and {fmt(st.animals)} dogs and cats found{#if s && s.failed} · {fmt(s.failed)} photos unreadable{/if}{/if}
          {#if st && !st.xmp}<br />Names stay in Hyperfocal (sidecar writing is off){/if}
        </div>
      {/snippet}
    </Popover>
  </div>

  <div class="scroll">
    {#if st && !people.enabled}
      <div class="empty-state">
        <div class="art"><Icon name="scan-face" size={44} /></div>
        <h2>People & pets</h2>
        <p>Hyperfocal can find the faces, dogs and cats in your photos, group them by who they are, and write the names you give them to the XMP sidecars (as <code>People|Name</code> and <code>Pets|Name</code> keywords, face regions and IPTC “person shown”), where Lightroom, digiKam and Excire read them.</p>
        <p>It needs the companion container <b>hyperfocal-ml</b>, which runs the models (on the CPU, or an NVIDIA GPU). Start it and set <code>HYPERFOCAL_ML_URL</code> on Hyperfocal, e.g. <code>http://hyperfocal-ml:8090</code>. Nothing leaves your server.</p>
      </div>
    {:else}
      {#if s && s.phase === 'offline' && s.error}
        <div class="notice"><Icon name="cloud-off" />The ML service at your <code>HYPERFOCAL_ML_URL</code> is not answering ({s.error}). Names and groups still work; new photos are analysed when it is back.</div>
      {/if}

      {#if people.named.length}
        <h3>{pets ? 'Pets' : 'People'}</h3>
        <div class="cards">
          {#each people.named as x (x.id)}
            <button class="card" onclick={() => openSheet(x)} title={`${subjectLabel(x)} · ${fmt(x.photos)} photos`}>
              <span class="face" class:pet={pets}>
                {#if x.cover}<img src={urls.crop(x.cover)} alt="" loading="lazy" draggable="false" />{/if}
                {#if x.review > 0}<span class="badge" title={`${x.review} to review`}>{x.review > 99 ? '99+' : x.review}</span>{/if}
              </span>
              <span class="name ellipsis">{x.name}</span>
              <span class="cnt dim tnum">{fmt(x.photos)} {x.photos === 1 ? 'photo' : 'photos'}</span>
            </button>
          {/each}
        </div>
      {/if}

      {#if people.groups.length}
        <h3>{people.named.length ? 'Not named yet' : pets ? 'Pets found' : 'People found'} <span class="dim tnum">{fmt(people.groupsTotal)}</span></h3>
        <p class="hint dim">Groups of {pets ? 'the same pet' : 'one person'}, most photos first. Name them to find them again and to write the names to the sidecars.</p>
        <div class="cards">
          {#each people.groups as x (x.id)}
            <button class="card" onclick={() => openSheet(x)} title={`${fmt(x.photos)} photos`}>
              <span class="face" class:pet={pets}>
                {#if x.cover}<img src={urls.crop(x.cover)} alt="" loading="lazy" draggable="false" />{/if}
              </span>
              <span class="name dim">{ro ? subjectLabel(x) : 'Add name'}</span>
              <span class="cnt dim tnum">{fmt(x.photos)} {x.photos === 1 ? 'photo' : 'photos'}</span>
            </button>
          {/each}
        </div>
        {#if people.groupsTotal > people.groups.length && !people.showSingles}
          <button class="btn sm ghost more" onclick={() => { people.showSingles = true; people.load(); }}>Show the groups of a single photo too</button>
        {/if}
      {/if}

      {#if people.showHidden && people.hidden.length}
        <h3>Hidden</h3>
        <div class="cards">
          {#each people.hidden as x (x.id)}
            <button class="card hidden" onclick={() => openSheet(x)}>
              <span class="face" class:pet={pets}>{#if x.cover}<img src={urls.crop(x.cover)} alt="" loading="lazy" draggable="false" />{/if}</span>
              <span class="name ellipsis">{subjectLabel(x)}</span>
              <span class="cnt dim tnum">{fmt(x.photos)} photos</span>
            </button>
          {/each}
        </div>
      {/if}

      {#if !people.loading && !people.named.length && !people.groups.length && st}
        <div class="empty-state small">
          <div class="art"><Icon name={pets ? 'paw' : 'users'} size={40} /></div>
          {#if s?.phase === 'analysing' || (s?.pending ?? 0) > 0}
            <p>No {pets ? 'pets' : 'people'} yet: the photos are still being analysed.</p>
          {:else}
            <p>No {pets ? 'dogs or cats' : 'faces'} found in more than one photo{people.showSingles ? '' : ' yet'}.</p>
          {/if}
        </div>
      {/if}
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
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
    min-height: var(--toolbar-h);
    flex-wrap: wrap;
  }
  .tabs > button {
    gap: 0.375rem;
  }
  .tabs .n {
    font-size: var(--fs-sm);
    opacity: 0.75;
    margin-left: 0.125rem;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--text-2);
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .state .spinner {
    width: 0.875rem;
    height: 0.875rem;
    flex: none;
  }
  .meter {
    width: 6rem;
    height: 0.3125rem;
    border-radius: 1rem;
    background: var(--surface-3);
    overflow: hidden;
    flex: none;
  }
  .meter span {
    display: block;
    height: 100%;
    background: var(--accent-strong);
  }
  .scroll {
    flex: 1;
    overflow: auto;
    padding: 0.5rem 1.25rem 3rem;
  }
  h3 {
    font-size: var(--fs-lg);
    margin: 1.25rem 0 0.75rem;
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
  }
  h3 .dim {
    font-size: var(--fs);
    font-weight: 500;
  }
  .hint {
    margin: -0.5rem 0 0.875rem;
    font-size: var(--fs-sm);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(7.5rem, 1fr));
    gap: 1.25rem 0.75rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    border: 0;
    background: none;
    cursor: pointer;
    padding: 0.25rem;
    border-radius: var(--r-lg);
    min-width: 0;
    color: var(--text);
  }
  .card:hover .face {
    box-shadow: 0 0 0 3px var(--accent);
  }
  .card.hidden {
    opacity: 0.6;
  }
  .face {
    position: relative;
    width: 6.5rem;
    height: 6.5rem;
    border-radius: 50%;
    background: var(--surface-2);
    transition: box-shadow 0.12s;
  }
  .face.pet {
    border-radius: 1.25rem;
  }
  .face img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: inherit;
  }
  .face .badge {
    position: absolute;
    right: 0.125rem;
    top: 0.125rem;
    box-shadow: 0 0 0 2px var(--bg);
  }
  .name {
    font-weight: 600;
    max-width: 100%;
    margin-top: 0.25rem;
  }
  .cnt {
    font-size: var(--fs-sm);
  }
  .more {
    margin-top: 1rem;
  }
  .notice {
    display: flex;
    gap: 0.625rem;
    align-items: flex-start;
    padding: 0.75rem 1rem;
    margin-top: 0.75rem;
    border-radius: var(--r-lg);
    background: var(--surface-2);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .notice :global(svg) {
    flex: none;
    color: var(--critical);
  }
  .empty-state {
    max-width: 36rem;
    margin: 3rem auto;
    text-align: center;
    color: var(--text-2);
    line-height: 1.55;
  }
  .empty-state h2 {
    color: var(--text);
    margin: 0.75rem 0;
  }
  .empty-state.small {
    margin-top: 4rem;
  }
  .art {
    width: 5rem;
    height: 5rem;
    border-radius: 50%;
    background: var(--accent-bg);
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
  @media (max-width: 760px) {
    .scroll {
      padding: 0.25rem 0.75rem 2rem;
    }
    .cards {
      grid-template-columns: repeat(auto-fill, minmax(5.75rem, 1fr));
      gap: 1rem 0.5rem;
    }
    .face {
      width: 5.25rem;
      height: 5.25rem;
    }
    .state .meter {
      display: none;
    }
  }
</style>
