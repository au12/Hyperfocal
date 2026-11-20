<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { api, type JournalEntry } from '../lib/api';
  import Icon from './Icon.svelte';

  let entries = $state<JournalEntry[]>([]);
  let loading = $state(true);

  async function load() {
    loading = true;
    entries = await api.journal().catch(() => []);
    loading = false;
  }
  onMount(load);

  async function undo(b: string) {
    await store.undo(b);
    load();
  }

  const icon = (k: string) => (k === 'reject' ? 'reject' : k === 'restore' ? 'restore' : 'tag');
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label="History">
    <header>
      <Icon name="history" />
      <h2>History</h2>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      {#if loading}<div class="spinner"></div>{/if}
      {#if !loading && entries.length === 0}<p class="dim">No changes yet.</p>{/if}
      {#each entries as e (e.batch)}
        <div class="je" class:undone={e.undone}>
          <Icon name={e.undo_of ? 'undo' : icon(e.kind)} />
          <div class="grow">
            <div>{e.summary || e.kind}{#if e.count > 1} <span class="dim">· {e.count} photos</span>{/if}</div>
            <div class="dim small">{new Date(e.ts).toLocaleString()}{#if e.undone}{' · undone'}{/if}</div>
          </div>
          {#if !e.undone && !store.status?.readonly}
            <button class="btn sm" onclick={() => undo(e.batch)}><Icon name="undo" /> Undo</button>
          {/if}
        </div>
      {/each}
      <p class="dim small foot">Sidecars are backed up before every change in the app's config folder (<code>backups/sidecars</code>).</p>
    </div>
  </div>
</div>

<style>
  .je {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.625rem 0;
    border-bottom: 1px solid var(--border);
  }
  .je :global(svg) {
    color: var(--text-3);
    flex: none;
  }
  .je.undone {
    opacity: 0.55;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .foot {
    margin-top: 1rem;
  }
  code {
    font-family: var(--mono);
  }
</style>
