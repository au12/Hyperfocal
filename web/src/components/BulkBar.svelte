<script lang="ts">
  import { store, LABELS } from '../lib/store.svelte';
  import Icon from './Icon.svelte';

  const n = $derived(store.selected.size);
  const anyRejected = $derived([...store.selected].some((id) => store.folders.get(store.list.f[store.index.get(id) ?? -1])?.isReject));
</script>

{#if n > 0 && !store.loupe && !store.compare}
  <div class="bulk" role="toolbar" aria-label="Selection actions">
    <span class="count tnum"><b>{n}</b> selected</span>
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
    transform: translateX(-50%);
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.375rem 0.5rem 0.375rem 0.875rem;
    background: var(--raised);
    border-radius: 1.5rem;
    box-shadow: var(--shadow-2);
    animation: pop-in 0.15s var(--ease);
    max-width: calc(100% - 1rem);
    white-space: nowrap;
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
