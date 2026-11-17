<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { ACTION_LABELS, keysFor, prettyKey } from '../lib/keys';
  import Icon from './Icon.svelte';
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)}>
  <div class="dialog help" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
    <header>
      <Icon name="keyboard" />
      <h2>Keyboard shortcuts</h2>
      <span class="dim small">{store.settings.keymap === 'frv' ? 'FastRawViewer layout' : 'Lightroom / Excire layout'}</span>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body cols">
      {#each ACTION_LABELS as [group, actions]}
        <section>
          <h3>{group}</h3>
          {#each actions as [a, label]}
            {@const keys = keysFor(a, store.settings.keymap)}
            {#if keys.length}
              <div class="kr"><span class="grow">{label}</span><span class="keys">{#each keys.slice(0, 2) as k}<kbd>{prettyKey(k)}</kbd>{/each}</span></div>
            {/if}
          {/each}
        </section>
      {/each}
      <section class="note">
        <h3>How changes are saved</h3>
        <p class="dim">Ratings, labels, picks, keywords, captions and locations go to standard <code>.xmp</code> sidecars that Lightroom, Excire and Capture One read. Rejects move the photo and its sidecar/JPEG into <code>{store.status?.reject_dir ?? '_Rejected'}</code> next to it. Original image files are never modified or deleted, and every change can be undone (Ctrl+Z).</p>
      </section>
    </div>
  </div>
</div>

<style>
  .dialog.help {
    width: min(68rem, 100%);
  }
  .cols {
    columns: 3 18rem;
    column-gap: 2rem;
  }
  section {
    break-inside: avoid;
    margin-bottom: 1rem;
  }
  h3 {
    margin: 0 0 0.375rem;
    font-size: var(--fs);
    color: var(--accent);
  }
  .kr {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 0;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .keys {
    display: flex;
    gap: 0.25rem;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .note p {
    font-size: var(--fs-sm);
    margin: 0;
  }
  code {
    font-family: var(--mono);
    font-size: 0.8em;
  }
</style>
