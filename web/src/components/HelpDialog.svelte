<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { ACTION_LABELS, keysFor, keyOf, prettyKey, ownerOf, assignKey, removeKey, resetAction, RESERVED, type Action } from '../lib/keys';
  import Icon from './Icon.svelte';

  const labels = new Map<Action, string>(ACTION_LABELS.flatMap(([, as]) => as));
  const MAX_KEYS = 4;

  let filter = $state('');
  /** The action waiting for a key press, and a key taken from another action awaiting
   * confirmation. */
  let capturing = $state<Action | null>(null);
  let pending = $state<{ action: Action; key: string; owner: Action } | null>(null);
  let notice = $state('');

  const preset = $derived(store.settings.keymap);
  const ov = $derived(store.keyOverrides());
  const changed = $derived(Object.keys(ov).length);

  const groups = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    return ACTION_LABELS.map(([g, as]) => [g, as.filter(([a, l]) => !q || l.toLowerCase().includes(q) || keysFor(a, preset, ov).some((k) => prettyKey(k).toLowerCase().includes(q)))] as const).filter(
      ([, as]) => as.length,
    );
  });

  const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'AltGraph', 'CapsLock']);

  // Key presses go to the editor first (capture phase), so they neither run the
  // action nor close the dialog.
  function onKey(e: KeyboardEvent) {
    if (pending) {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopImmediatePropagation();
        pending = null;
      } else if (e.key === 'Enter') {
        e.preventDefault();
        e.stopImmediatePropagation();
        confirm();
      }
      return;
    }
    if (!capturing) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (e.key === 'Escape') {
      capturing = null;
      notice = '';
      return;
    }
    if (MODIFIERS.has(e.key)) return;
    const key = keyOf(e);
    if (!key || RESERVED.has(key)) {
      notice = `${e.key || e.code} cannot be used as a shortcut`;
      return;
    }
    const action = capturing;
    capturing = null;
    notice = '';
    const owner = ownerOf(key, preset, ov);
    if (owner === action) return;
    if (owner) {
      pending = { action, key, owner };
      return;
    }
    store.setKeyOverrides(assignKey(preset, ov, action, key));
  }

  $effect(() => {
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  function confirm() {
    if (!pending) return;
    store.setKeyOverrides(assignKey(preset, ov, pending.action, pending.key));
    pending = null;
  }

  function capture(a: Action) {
    pending = null;
    notice = '';
    capturing = capturing === a ? null : a;
  }

  function resetAll() {
    const before = ov;
    store.setKeyOverrides({});
    store.toast('Shortcuts reset to the defaults of this layout', 'ok', { label: 'Undo', run: () => store.setKeyOverrides(before) });
  }

  function setPreset(p: 'frv' | 'lr') {
    store.settings.keymap = p;
    store.persist();
    capturing = null;
    pending = null;
  }

  function close() {
    store.dialog = null;
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="dialog help" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
    <header>
      <Icon name="keyboard" />
      <h2>Keyboard shortcuts</h2>
      <button class="btn ghost icon" onclick={close} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="bar">
      <div class="seg" role="group" aria-label="Layout">
        <button class:on={preset === 'frv'} onclick={() => setPreset('frv')}>FastRawViewer</button>
        <button class:on={preset === 'lr'} onclick={() => setPreset('lr')}>Lightroom / Excire</button>
      </div>
      <input class="field small search" type="search" placeholder="Find an action or key" bind:value={filter} aria-label="Find an action or key" />
      <span class="grow"></span>
      {#if changed}<span class="dim small">{changed} changed</span>{/if}
      <button class="btn sm" onclick={resetAll} disabled={!changed}><Icon name="undo" />Reset all to defaults</button>
    </div>
    {#if pending}
      {@const p = pending}
      <div class="prompt" role="alertdialog" aria-live="assertive">
        <span><kbd>{prettyKey(p.key)}</kbd> is the shortcut for <b>{labels.get(p.owner) ?? p.owner}</b>. Use it for <b>{labels.get(p.action) ?? p.action}</b> instead?</span>
        <span class="grow"></span>
        <button class="btn sm primary" onclick={confirm}>Reassign</button>
        <button class="btn sm ghost" onclick={() => (pending = null)}>Cancel</button>
      </div>
    {:else if notice}
      <div class="prompt warn" aria-live="polite"><span>{notice}</span></div>
    {/if}
    <p class="hint dim small">Click <b>+</b> and press the new key; <b>×</b> removes a key. Changes apply to the {preset === 'frv' ? 'FastRawViewer' : 'Lightroom / Excire'} layout and are kept in this browser.</p>
    <div class="body cols">
      {#each groups as [group, actions] (group)}
        <section>
          <h3>{group}</h3>
          {#each actions as [a, label] (a)}
            {@const keys = keysFor(a, preset, ov)}
            <div class="kr" class:editing={capturing === a} class:changed={a in ov}>
              <span class="grow">{label}</span>
              <span class="keys">
                {#each keys as k (k)}
                  <span class="key">
                    <kbd>{prettyKey(k)}</kbd>
                    <button class="rm" onclick={() => store.setKeyOverrides(removeKey(preset, ov, a, k))} aria-label={`Remove ${prettyKey(k)} from ${label}`} title="Remove">×</button>
                  </span>
                {/each}
                {#if capturing === a}
                  <span class="listen" aria-live="polite">Press a key…</span>
                {:else if keys.length < MAX_KEYS}
                  <button class="add" onclick={() => capture(a)} aria-label={`Add a shortcut for ${label}`} title="Add a shortcut">+</button>
                {/if}
                {#if a in ov}
                  <button class="reset" onclick={() => store.setKeyOverrides(resetAction(preset, ov, a))} aria-label={`Reset ${label} to its default keys`} title="Default keys"><Icon name="undo" size={13} /></button>
                {/if}
              </span>
            </div>
          {/each}
        </section>
      {/each}
      {#if !groups.length}<p class="dim">No action matches “{filter}”.</p>{/if}
      <section class="note">
        <h3>How changes are saved</h3>
        <p class="dim">Ratings, labels, picks, keywords, captions and locations go to standard <code>.xmp</code> sidecars that Lightroom, Excire and Capture One read. Rejects move the photo and its sidecar/JPEG into <code>{store.status?.reject_dir ?? '_Rejected'}</code> next to it. Original image files are never modified or deleted, and every change can be undone{store.keyHint('undo')}.</p>
      </section>
    </div>
  </div>
</div>

<style>
  .dialog.help {
    width: min(72rem, 100%);
  }
  .bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.625rem 1.25rem 0;
  }
  .bar .seg > button {
    height: 1.875rem;
    font-size: var(--fs-sm);
  }
  .search {
    width: 14rem;
  }
  .grow {
    flex: 1;
  }
  .prompt {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin: 0.625rem 1.25rem 0;
    padding: 0.5rem 0.75rem;
    border-radius: var(--r);
    background: color-mix(in srgb, var(--accent) 14%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    font-size: var(--fs-sm);
  }
  .prompt.warn {
    background: color-mix(in srgb, #e5484d 12%, var(--surface));
    border-color: color-mix(in srgb, #e5484d 40%, transparent);
  }
  .hint {
    margin: 0.5rem 1.25rem 0;
  }
  .cols {
    columns: 3 20rem;
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
    padding: 0.1875rem 0;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
    min-height: 1.875rem;
  }
  .kr.changed > .grow {
    font-weight: 600;
  }
  .kr.editing {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .keys {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.25rem;
  }
  .key {
    position: relative;
    display: inline-flex;
  }
  .rm {
    position: absolute;
    top: -0.4375rem;
    right: -0.4375rem;
    width: 1rem;
    height: 1rem;
    border-radius: 50%;
    border: 0;
    background: var(--danger, #c4372f);
    color: #fff;
    font-size: 0.75rem;
    line-height: 1rem;
    padding: 0;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.1s;
  }
  .key:hover .rm,
  .rm:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .rm {
      opacity: 0.85;
    }
  }
  .add,
  .reset {
    width: 1.5rem;
    height: 1.5rem;
    border-radius: 0.375rem;
    border: 1px dashed var(--border-strong);
    background: none;
    color: var(--text-2);
    cursor: pointer;
    display: grid;
    place-items: center;
    padding: 0;
    font-size: 0.9375rem;
  }
  .add:hover,
  .reset:hover {
    border-style: solid;
    color: var(--text);
  }
  .reset {
    border: 0;
  }
  .listen {
    font-size: 0.6875rem;
    color: var(--accent);
    font-weight: 600;
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.45;
    }
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
  @media (max-width: 760px) {
    .search {
      width: 100%;
      order: 3;
    }
  }
</style>
