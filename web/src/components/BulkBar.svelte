<script lang="ts">
  import { store, LABELS } from '../lib/store.svelte';
  import { rise } from '../lib/motion';
  import { albums } from '../lib/albums.svelte';
  import Icon from './Icon.svelte';

  const n = $derived(store.selected.size);
  const anyRejected = $derived([...store.selected].some((id) => store.folders.get(store.list.f[store.index.get(id) ?? -1])?.isReject));
</script>

{#if n > 0 && !store.loupe && !store.compare}
  <div class="bulk glass" role="toolbar" aria-label="Selection actions" transition:rise={{ y: 24, scale: 0.94, duration: 320 }}>
    <span class="count tnum">{#key n}<b class="num">{n}</b>{/key} selected</span>
    <div class="stars">
      {#each [1, 2, 3, 4, 5] as r}<button onclick={() => store.rate(r)} title={`${r}★`}>★</button>{/each}
    </div>
    <div class="labels">
      {#each LABELS as l}<button onclick={() => store.label(l.color)} title={l.name}><span class="label-dot label-{l.color}"></span></button>{/each}
    </div>
    {#if n >= 2}
      <button class="btn sm" onclick={() => store.openCompare()} title={(n > 4 ? 'Compare the first 4' : 'Compare side by side') + store.keyHint('compare')}><Icon name={n >= 4 ? 'compare-4' : 'compare'} />{store.mobile ? '' : 'Compare'}</button>
    {/if}
    <button class="btn sm" onclick={() => store.togglePick(1)} title="Pick"><Icon name="flag" />{store.mobile ? '' : 'Pick'}</button>
    <button class="btn sm" onclick={() => store.openLocation()} title="Set location"><Icon name="pin" />{store.mobile ? '' : 'Location'}</button>
    <button class="btn sm" onclick={() => albums.pick()} title={'Add to an album' + store.keyHint('albumPick')}><Icon name="album-plus" />{store.mobile ? '' : 'Album'}</button>
    {#if albums.currentId != null}
      <button class="btn sm" onclick={() => albums.remove()} title={'Take out of this album (the files stay)' + store.keyHint('albumRemove')}><Icon name="album-minus" />{store.mobile ? '' : 'Take out'}</button>
    {/if}
    <button class="btn sm {anyRejected ? '' : 'danger'}" onclick={() => (anyRejected ? store.restore() : store.reject())}>
      <Icon name={anyRejected ? 'restore' : 'reject'} />{store.mobile ? '' : anyRejected ? 'Restore' : 'Reject'}
    </button>
    <button class="btn sm ghost icon" onclick={() => store.clearSelection()} title={'Clear selection' + store.keyHint('selectNone')}><Icon name="x" /></button>
  </div>
{/if}

<style>
  .bulk {
    position: absolute;
    left: 50%;
    bottom: 1rem;
    translate: -50% 0;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3125rem 0.375rem 0.3125rem 0.9375rem;
    border-radius: 999px;
    box-shadow: var(--shadow-2);
    max-width: calc(100% - 1rem);
    white-space: nowrap;
  }
  .num {
    display: inline-block;
    animation: pop-once 0.36s var(--ease-spring);
  }
  .count {
    margin-right: 0.25rem;
  }
  .stars button {
    border: 0;
    background: none;
    color: var(--star);
    font-size: 1.125rem;
    cursor: pointer;
    padding: 0 0.0625rem;
    opacity: 0.85;
  }
  .stars button:hover {
    opacity: 1;
    transform: scale(1.15);
  }
  /* Hover previews the rating: stars after the hovered one dim. */
  @media (hover: hover) {
    .stars:has(button:hover) button {
      opacity: 0.3;
    }
    .stars:has(button:hover) button:hover,
    .stars:has(button:hover) button:has(~ button:hover) {
      opacity: 1;
    }
  }
  .labels {
    display: flex;
    gap: 0.125rem;
  }
  .labels button {
    border: 0;
    background: none;
    width: 1.375rem;
    height: 1.375rem;
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  @media (max-width: 760px) {
    .bulk {
      bottom: 0.5rem;
      gap: 0.25rem;
      padding: 0.25rem 0.375rem 0.25rem 0.625rem;
    }
    .labels {
      display: none;
    }
  }
</style>
