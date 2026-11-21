<script lang="ts">
  import { untrack } from 'svelte';
  import { urls, type Detection, type PhotoPeople } from '../lib/api';
  import { people } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';
  import NameInput from './NameInput.svelte';

  /** Who is in a photo: names, suggestions to confirm, unknown faces to name. */
  let { photo }: { photo: number } = $props();

  let data = $state<PhotoPeople | null>(null);
  let naming = $state<number | null>(null);
  let shown = -1;

  async function load(id: number) {
    try {
      const d = await people.photo(id);
      if (id === photo) data = d;
    } catch {
      if (id === photo) data = null;
    }
  }

  $effect(() => {
    const id = photo;
    void people.rev;
    untrack(() => {
      if (id !== shown) {
        shown = id;
        data = null;
        naming = null;
        load(id);
      } else if (people.affects(id)) load(id);
    });
  });

  const dets = $derived((data?.detections ?? []).filter((d) => !d.who?.hidden));
  const hiddenCount = $derived((data?.detections ?? []).length - dets.length);

  function name(d: Detection, p: { subject?: number; name: string }) {
    naming = null;
    people.assign([d.id], p.subject != null ? { subject: p.subject } : { name: p.name }, p.name);
  }

  function show(d: Detection) {
    if (!d.who) return;
    store.loupe = false;
    store.showSubject(d.who.id, d.who.name ?? 'Unnamed');
  }

  const what = (d: Detection) => (d.kind === 'face' ? 'face' : d.kind);
</script>

{#if data}
  {#if !data.analyzed}
    <div class="dim small row">
      {#if data.error}
        <Icon name="info" size={14} /> Not analysed: {data.error}
      {:else if people.status?.status.enabled && people.status.status.phase === 'preparing'}
        <span class="spinner sm"></span> Waiting for the AI models…
      {:else if people.status?.status.enabled}
        <span class="spinner sm"></span> Looking for faces and pets…
      {:else}
        Not analysed
      {/if}
    </div>
  {/if}
  {#each dets as d (d.id)}
    {@const who = d.who}
    <div class="p">
      <img class="crop" class:pet={d.kind !== 'face'} src={urls.crop(d)} alt="" loading="lazy" />
      {#if naming === d.id}
        <div class="grow"><NameInput kind={d.kind} compact onpick={(p) => name(d, p)} oncancel={() => (naming = null)} /></div>
      {:else if who?.name && d.state === 1}
        <button class="nm grow ellipsis" onclick={() => show(d)} title={`Photos of ${who.name}`}>{who.name}</button>
        <button class="btn sm ghost icon" onclick={() => (naming = d.id)} title="Someone else" aria-label="Someone else"><Icon name="edit" /></button>
        <button class="btn sm ghost icon" onclick={() => people.reject([d.id])} title={`Not ${who.name}`} aria-label={`Not ${who.name}`}><Icon name="user-x" /></button>
      {:else if who?.name}
        <span class="grow ellipsis"><span class="nm sug">{who.name}?</span></span>
        <button class="btn sm icon yes" onclick={() => people.confirm({ ids: [d.id] })} title={`Yes, ${who.name}`} aria-label="Yes"><Icon name="check" /></button>
        <button class="btn sm icon" onclick={() => people.reject([d.id])} title={`Not ${who.name}`} aria-label="No"><Icon name="x" /></button>
        <button class="btn sm ghost icon" onclick={() => (naming = d.id)} title="Someone else" aria-label="Someone else"><Icon name="edit" /></button>
      {:else}
        <span class="grow dim ellipsis">{who ? `Unnamed ${what(d) === 'face' ? 'person' : what(d)}` : `Unknown ${what(d)}`}</span>
        <button class="btn sm" onclick={() => (naming = d.id)}>Name</button>
        <button class="btn sm ghost icon" onclick={() => people.ignore([d.id])} title={d.kind === 'face' ? 'Not a face, or not of interest' : 'Not of interest'} aria-label="Ignore"><Icon name="eye-off" /></button>
      {/if}
    </div>
  {/each}
  {#if data.analyzed && dets.length === 0}
    <div class="dim small">No faces or pets found{hiddenCount ? ` (${hiddenCount} hidden)` : ''}.</div>
  {/if}
{/if}

<style>
  .p {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-height: 2.5rem;
  }
  .crop {
    width: 2.25rem;
    height: 2.25rem;
    border-radius: 50%;
    object-fit: cover;
    background: var(--surface-2);
    flex: none;
  }
  .crop.pet {
    border-radius: 0.5rem;
  }
  .nm {
    border: 0;
    background: none;
    padding: 0;
    text-align: left;
    font-weight: 600;
    color: var(--text);
    cursor: pointer;
  }
  button.nm:hover {
    color: var(--accent);
  }
  .sug {
    color: var(--text-2);
    font-style: italic;
    cursor: default;
  }
  .yes {
    color: var(--positive);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .spinner.sm {
    width: 0.875rem;
    height: 0.875rem;
  }
</style>
