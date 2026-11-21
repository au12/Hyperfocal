<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { people } from '../lib/people.svelte';
  import { segmented } from '../lib/motion';
  import Icon from './Icon.svelte';
</script>

<nav class="mnav glass" use:segmented>
  <button class:on={store.view === 'grid'} onclick={() => (store.view = 'grid')}><Icon name="grid" size={22} /><span>Photos</span></button>
  <button class:on={store.view === 'map'} onclick={() => (store.view = 'map')}><Icon name="map" size={22} /><span>Map</span></button>
  {#if people.enabled}
    <button class:on={store.view === 'people'} onclick={() => store.openPeople()}><Icon name="users" size={22} /><span>People</span></button>
  {/if}
  <button class="sel" class:active={store.selected.size > 0} onclick={() => (store.selected.size ? store.clearSelection() : store.select(store.cursor, 'toggle'))} disabled={store.view === 'people'}>
    <Icon name="selection" size={22} /><span class="tnum">{store.selected.size ? `${store.selected.size} sel.` : 'Select'}</span>
  </button>
  <button onclick={() => (store.dialog = 'journal')}><Icon name="history" size={22} /><span>History</span></button>
</nav>

<style>
  .mnav {
    position: relative;
    flex: none;
    display: flex;
    justify-content: space-around;
    border-top: 1px solid var(--border);
    padding: 0.3125rem 0 calc(0.3125rem + env(safe-area-inset-bottom));
    isolation: isolate;
  }
  /* The pill behind the current tab's icon slides from tab to tab (motion.ts). */
  .mnav :global(.seg-ind) {
    position: absolute;
    z-index: -1;
    left: 0;
    top: 0.3125rem;
    height: 1.875rem;
    width: 0;
    pointer-events: none;
    transition:
      transform var(--dur-slow) var(--ease-out),
      width var(--dur-slow) var(--ease-out),
      opacity var(--dur);
  }
  .mnav :global(.seg-ind)::before {
    content: '';
    position: absolute;
    left: 50%;
    top: 0;
    width: 3.5rem;
    height: 100%;
    transform: translateX(-50%);
    border-radius: 1rem;
    background: var(--accent-bg-strong);
  }
  button {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1875rem;
    border: 0;
    background: none;
    color: var(--text-2);
    font-size: 0.6875rem;
    font-weight: 600;
    padding: 0.25rem 0 0.125rem;
    transition: color var(--dur-fast);
  }
  button :global(svg) {
    height: 1.875rem;
    padding: 0.25rem 0;
    box-sizing: border-box;
    transition: transform var(--dur-fast) var(--ease-out);
  }
  button:active:not(:disabled) :global(svg) {
    transform: scale(0.86);
  }
  button.on {
    color: var(--accent);
  }
  button.on :global(svg) {
    animation: pop-once 0.38s var(--ease-spring);
  }
  button.sel.active {
    color: var(--accent);
  }
  button:disabled {
    opacity: 0.4;
  }
</style>
