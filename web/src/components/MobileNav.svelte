<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { people } from '../lib/people.svelte';
  import Icon from './Icon.svelte';
</script>

<nav class="mnav">
  <button class:on={store.view === 'grid'} onclick={() => (store.view = 'grid')}><Icon name="grid" size={22} /><span>Photos</span></button>
  <button class:on={store.view === 'map'} onclick={() => (store.view = 'map')}><Icon name="map" size={22} /><span>Map</span></button>
  {#if people.enabled}
    <button class:on={store.view === 'people'} onclick={() => store.openPeople()}><Icon name="users" size={22} /><span>People</span></button>
  {/if}
  <button onclick={() => (store.selected.size ? store.clearSelection() : store.select(store.cursor, 'toggle'))} disabled={store.view === 'people'}>
    <Icon name="selection" size={22} /><span>{store.selected.size ? `${store.selected.size} sel.` : 'Select'}</span>
  </button>
  <button onclick={() => (store.dialog = 'journal')}><Icon name="history" size={22} /><span>History</span></button>
</nav>

<style>
  .mnav {
    flex: none;
    display: flex;
    justify-content: space-around;
    border-top: 1px solid var(--border);
    background: var(--shell);
    padding: 0.25rem 0 calc(0.25rem + env(safe-area-inset-bottom));
  }
  button {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.125rem;
    border: 0;
    background: none;
    color: var(--text-2);
    font-size: 0.6875rem;
    font-weight: 600;
    padding: 0.375rem 0;
  }
  button.on {
    color: var(--accent);
  }
  button:disabled {
    opacity: 0.4;
  }
</style>
