<script lang="ts">
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte';
  import { api, type ImmichAlbum } from '../lib/api';
  import { segmented, rise } from '../lib/motion';
  import Icon from './Icon.svelte';

  // Send to Immich: the photos go to the Immich server (those it has already are used
  // as they are), into an album if asked. RAW files go as a full-size JPEG or as the
  // original; nothing here changes.

  const ids = store.immichTargets.ids;
  const suggested = store.immichTargets.album?.trim() ?? '';
  const raws = ids.filter((id) => store.list.k[store.index.get(id) ?? -1] === 1).length;
  const library = !!store.status?.immich?.library;

  let albums = $state<ImmichAlbum[] | null>(null);
  let error = $state('');
  let choice = $state<'none' | 'new' | 'existing'>(suggested ? 'new' : 'none');
  let newName = $state(suggested);
  let picked = $state<string | null>(null);
  let q = $state('');
  let mode = $state<'jpeg' | 'original'>(localStorage.getItem('hf.immich.mode') === 'original' ? 'original' : 'jpeg');
  let busy = $state(false);

  onMount(async () => {
    try {
      albums = await api.immichAlbums();
      // An album of that name already in Immich: add to it rather than make a second.
      const same = suggested && albums.find((a) => a.name.toLowerCase() === suggested.toLowerCase());
      if (same) {
        choice = 'existing';
        picked = same.id;
      }
    } catch (e) {
      error = (e as Error).message;
      albums = [];
    }
  });

  const shown = $derived((albums ?? []).filter((a) => !q.trim() || a.name.toLowerCase().includes(q.trim().toLowerCase())));
  const ready = $derived(!busy && !error && (choice === 'none' || (choice === 'new' && newName.trim().length > 0) || (choice === 'existing' && picked != null)));

  function close() {
    store.dialog = null;
  }

  async function send() {
    if (!ready) return;
    busy = true;
    try {
      localStorage.setItem('hf.immich.mode', mode);
    } catch {
      /* private mode */
    }
    try {
      const { job } = await api.immichSend({
        ids,
        mode,
        album: choice === 'existing' ? (picked ?? undefined) : undefined,
        album_name: choice === 'new' ? newName.trim() : undefined,
      });
      store.onJob(job);
      store.toast(`Sending ${ids.length === 1 ? 'the photo' : `${ids.length.toLocaleString()} photos`} to Immich…`, 'info', undefined, 3000);
      close();
    } catch (e) {
      error = (e as Error).message;
    } finally {
      busy = false;
    }
  }

  const fmt = (n: number) => n.toLocaleString();
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="dialog immich" role="dialog" aria-modal="true" aria-label="Send to Immich">
    <header>
      <Icon name="send" />
      <h2>Send {ids.length === 1 ? 'this photo' : `${fmt(ids.length)} photos`} to Immich</h2>
      <button class="btn ghost icon" onclick={close} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      {#if error}
        <div class="err" transition:rise={{ y: -4 }}><Icon name="info" size={16} /><span>{error}</span></div>
      {/if}

      <div class="fl">Album</div>
      <div class="choices">
        <label class="choice" class:on={choice === 'none'}>
          <input type="radio" bind:group={choice} value="none" />
          <span class="dot"></span>
          <span class="grow"><b>No album</b><span class="dim">Just add them to the timeline</span></span>
        </label>
        <label class="choice" class:on={choice === 'new'}>
          <input type="radio" bind:group={choice} value="new" />
          <span class="dot"></span>
          <span class="grow"><b>A new album</b>
            {#if choice === 'new'}
              <!-- svelte-ignore a11y_autofocus -->
              <input class="field" bind:value={newName} placeholder="Album name" maxlength="200" autofocus onkeydown={(e) => e.key === 'Enter' && send()} />
            {:else}
              <span class="dim">Named as you like</span>
            {/if}
          </span>
        </label>
        <label class="choice" class:on={choice === 'existing'}>
          <input type="radio" bind:group={choice} value="existing" />
          <span class="dot"></span>
          <span class="grow"><b>An album in Immich</b><span class="dim">{albums == null ? 'Looking…' : albums.length ? `${fmt(albums.length)} to choose from` : 'None yet'}</span></span>
        </label>
      </div>

      {#if choice === 'existing'}
        <div class="albums" transition:rise={{ y: -6, duration: 240 }}>
          {#if (albums?.length ?? 0) > 6}
            <div class="find"><Icon name="search" size={15} /><input bind:value={q} placeholder="Find an album" /></div>
          {/if}
          <div class="list" role="listbox" aria-label="Immich albums">
            {#if albums == null}
              {#each [0, 1, 2] as k (k)}<div class="arow skeleton" style:opacity={1 - k * 0.25}></div>{/each}
            {:else}
              {#each shown as a, k (a.id)}
                <button class="arow" class:sel={picked === a.id} role="option" aria-selected={picked === a.id} onclick={() => (picked = a.id)} in:rise={{ y: 4, duration: 220, delay: Math.min(k, 8) * 18 }}>
                  <Icon name="album" size={16} />
                  <span class="grow ellipsis">{a.name || 'Untitled'}</span>
                  <span class="dim tnum">{fmt(a.count)}</span>
                  {#if picked === a.id}<Icon name="check" size={15} stroke={2.5} />{/if}
                </button>
              {:else}
                <div class="dim none">No album matches.</div>
              {/each}
            {/if}
          </div>
        </div>
      {/if}

      {#if raws > 0}
        <div class="fl">RAW files {raws < ids.length ? `(${fmt(raws)} of them)` : ''} go as</div>
        <div class="seg" use:segmented>
          <button class:on={mode === 'jpeg'} onclick={() => (mode = 'jpeg')}>Full-size JPEG</button>
          <button class:on={mode === 'original'} onclick={() => (mode = 'original')}>Original RAW</button>
        </div>
        <p class="note dim">
          {#if mode === 'jpeg'}
            The camera's own JPEG of a RAW+JPEG pair, or the full-size JPEG inside the RAW, with the photo's capture time, camera, GPS, rating and keywords. They look in Immich as they do here, at a fraction of the size.
          {:else}
            The RAW files themselves, with their XMP sidecars. Immich renders them its own way.
          {/if}
        </p>
      {/if}
      <p class="note dim">
        <Icon name="shield-check" size={14} />
        {library ? 'Photos Immich already reads from this library, or has from before, are used as they are: nothing is uploaded twice.' : 'Photos Immich already has (the same file) are used as they are: nothing is uploaded twice.'}
        Nothing in your library changes.
      </p>
    </div>
    <footer>
      <button class="btn" onclick={close}>Cancel</button>
      <button class="btn primary" onclick={send} disabled={!ready}>
        {#if busy}<span class="spinner light"></span>{:else}<Icon name="send" />{/if}
        Send {ids.length === 1 ? '' : fmt(ids.length)}
      </button>
    </footer>
  </div>
</div>

<style>
  .immich {
    width: min(30rem, 100%);
  }
  .fl {
    font-size: var(--fs-xs);
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-3);
    margin: 0.25rem 0 0.5rem;
  }
  .fl:not(:first-child) {
    margin-top: 1.125rem;
  }
  .err {
    display: flex;
    gap: 0.5rem;
    align-items: flex-start;
    padding: 0.625rem 0.75rem;
    margin-bottom: 0.75rem;
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 12%, transparent);
    color: var(--negative);
    font-size: var(--fs-sm);
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .choice {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.625rem 0.75rem;
    border-radius: var(--r-lg);
    box-shadow: inset 0 0 0 1px var(--border);
    cursor: pointer;
    transition:
      box-shadow var(--dur),
      background-color var(--dur-fast);
  }
  .choice:hover {
    background: var(--surface-2);
  }
  .choice.on {
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .choice input[type='radio'] {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .dot {
    width: 1rem;
    height: 1rem;
    margin-top: 0.125rem;
    flex: none;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
    transition: box-shadow var(--dur) var(--ease-spring);
  }
  .choice.on .dot {
    box-shadow:
      inset 0 0 0 1.5px var(--accent),
      inset 0 0 0 4.5px var(--surface),
      inset 0 0 0 10px var(--accent);
  }
  .choice:has(input:focus-visible) {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .grow {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    min-width: 0;
  }
  .grow .dim {
    font-size: var(--fs-sm);
  }
  .grow .field {
    margin-top: 0.375rem;
  }
  .albums {
    margin-top: 0.5rem;
  }
  .find {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0 0.625rem;
    height: 2rem;
    border-radius: var(--r);
    box-shadow: inset 0 0 0 1px var(--field-border);
    color: var(--text-3);
    margin-bottom: 0.375rem;
  }
  .find input {
    flex: 1;
    border: 0;
    background: none;
    outline: none;
    color: var(--text);
  }
  .list {
    max-height: 13rem;
    overflow: auto;
    border-radius: var(--r-lg);
    box-shadow: inset 0 0 0 1px var(--border);
    padding: 0.25rem;
  }
  .arow {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    width: 100%;
    min-height: 2.125rem;
    padding: 0 0.625rem;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    transition: background-color var(--dur-fast);
  }
  .arow :global(svg) {
    color: var(--text-3);
    flex: none;
  }
  .arow:hover {
    background: var(--surface-2);
  }
  .arow.sel {
    background: var(--accent-bg);
  }
  .arow.sel :global(svg) {
    color: var(--accent);
  }
  .arow .grow {
    flex: 1;
    display: block;
  }
  .arow.skeleton {
    margin: 0.25rem 0;
  }
  .none {
    padding: 0.75rem;
    text-align: center;
    font-size: var(--fs-sm);
  }
  .note {
    font-size: var(--fs-sm);
    margin: 0.625rem 0 0;
    line-height: 1.5;
  }
  .note :global(svg) {
    vertical-align: -2px;
    color: var(--positive);
  }
  .spinner.light {
    width: 0.875rem;
    height: 0.875rem;
    background: conic-gradient(from 0deg, transparent 0 25%, #fff 100%);
  }
</style>
