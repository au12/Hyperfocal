<script lang="ts">
  // Debug mode's tab on the left edge: says the telemetry is recording, and marks a moment
  // ("that was slow") in it, with a note, from anywhere, the single view included.
  import { store } from '../lib/store.svelte';
  import { tel, MARK_KEY } from '../lib/telemetry';
  import { keyOf, ownerOf, prettyKey } from '../lib/keys';
  import { settingsNav } from './SettingsDialog.svelte';
  import Icon from './Icon.svelte';

  let open = $state(false);
  let note = $state('');
  let busy = $state(false);
  let input = $state<HTMLInputElement | null>(null);
  let pop = $state<HTMLElement | null>(null);
  let hidden = $state(sessionStorage.getItem('hf.debugTab') === 'hidden');
  let me = $state(tel.snapshot());
  const markShortcut = prettyKey(MARK_KEY);

  function show() {
    open = true;
    me = tel.snapshot();
    queueMicrotask(() => input?.focus());
  }

  async function mark() {
    busy = true;
    const ok = await tel.mark(note.trim());
    busy = false;
    store.toast(ok ? 'Moment marked in the telemetry' : 'Could not mark the moment', ok ? 'ok' : 'error');
    if (ok) {
      note = '';
      open = false;
    }
  }

  function panel() {
    open = false;
    settingsNav.section = 'debug';
    store.dialog = 'settings';
  }

  function hide() {
    hidden = true;
    open = false;
    sessionStorage.setItem('hf.debugTab', 'hidden');
    store.toast(`Debug tab hidden for this session (${markShortcut} still marks a moment)`, 'info');
  }

  function onKey(e: KeyboardEvent) {
    if (keyOf(e) !== MARK_KEY || ownerOf(MARK_KEY, store.settings.keymap, store.keyOverrides())) return;
    const t = e.target as HTMLElement | null;
    if (t && !pop?.contains(t) && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
    e.preventDefault();
    e.stopPropagation();
    if (open) open = false;
    else show();
  }

  function onDown(e: PointerEvent) {
    if (open && pop && !pop.contains(e.target as Node)) open = false;
  }
</script>

<svelte:window onkeydowncapture={onKey} onpointerdowncapture={onDown} />

{#if !hidden || open}
  <button class="debug-tab" class:open title={`Debug mode is recording · mark a moment (${markShortcut})`} aria-label="Debug mode: mark a moment" onclick={() => (open ? (open = false) : show())}>
    <Icon name="bug" size={15} />
  </button>
{/if}

{#if open}
  <div class="pop" bind:this={pop} role="dialog" aria-label="Mark a moment">
    <div class="head"><span class="dot"></span>Debug mode is recording</div>
    <form class="row" onsubmit={(e) => (e.preventDefault(), mark())}>
      <input
        class="field"
        bind:this={input}
        bind:value={note}
        maxlength="1000"
        placeholder="What happened? (optional)"
        onkeydown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation();
            open = false;
          }
        }}
      />
      <button class="btn primary sm" type="submit" disabled={busy}>Mark</button>
    </form>
    <p class="dim tnum">
      This page: {me.errors} error{me.errors === 1 ? '' : 's'} · {me.mb} MB · {me.long} long task{me.long === 1 ? '' : 's'}{#if me.jsMb != null}{' '}· JS {Math.round(me.jsMb)} MB{/if}
    </p>
    <div class="row">
      <button class="btn sm" onclick={panel}><Icon name="gauge" />Debug panel</button>
      {#if !hidden}<button class="btn sm ghost" onclick={hide}>Hide tab</button>{/if}
    </div>
  </div>
{/if}

<style>
  .debug-tab {
    position: fixed;
    left: 0;
    top: 68%;
    z-index: 90;
    width: 1.375rem;
    height: 2.25rem;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-left: 0;
    border-radius: 0 var(--r) var(--r) 0;
    background: var(--shell);
    color: var(--critical);
    opacity: 0.55;
    cursor: pointer;
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    transition: opacity var(--dur-fast), width var(--dur-fast);
  }
  .debug-tab:hover,
  .debug-tab.open {
    opacity: 1;
    width: 1.75rem;
  }
  .pop {
    position: fixed;
    left: 2.25rem;
    top: calc(68% - 4rem);
    z-index: 91;
    width: min(22rem, calc(100vw - 3rem));
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-2);
    animation: fade var(--dur-fast) var(--ease-out);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 600;
    font-size: var(--fs-sm);
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--negative);
    animation: pulse 2s infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
  .field {
    flex: 1;
    min-width: 0;
    height: 2rem;
  }
  p {
    margin: 0;
    font-size: var(--fs-xs);
  }
</style>
