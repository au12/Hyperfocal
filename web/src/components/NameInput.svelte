<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api, type DetKind } from '../lib/api';
  import Icon from './Icon.svelte';

  /** A name for a face or an animal: an existing person or pet (suggested as you type,
   * most photographed first), or a new one. */
  let {
    kind,
    value = '',
    placeholder,
    exclude,
    autofocus = true,
    compact = false,
    onpick,
    oncancel,
  }: {
    kind: DetKind | 'pet';
    value?: string;
    placeholder?: string;
    /** A subject not to offer (the one being renamed). */
    exclude?: number;
    autofocus?: boolean;
    compact?: boolean;
    onpick: (p: { subject?: number; name: string }) => void;
    oncancel?: () => void;
  } = $props();

  type Name = { id: number; name: string; kind: string; photos: number };
  // The starting text; typing takes over from there.
  let text = $state(untrack(() => value));
  let sugg = $state<Name[]>([]);
  let active = $state(-1);
  let open = $state(false);
  let input: HTMLInputElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctl: AbortController | null = null;

  const isPet = $derived(kind !== 'face');
  const trimmed = $derived(text.trim().replace(/\s+/g, ' '));
  const exact = $derived(sugg.find((s) => s.name.toLowerCase() === trimmed.toLowerCase()));

  onMount(() => {
    if (autofocus) requestAnimationFrame(() => input?.focus());
    return () => clearTimeout(timer);
  });

  function fetchNames() {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      ctl?.abort();
      ctl = new AbortController();
      try {
        const r = await api.peopleNames(trimmed, kind, ctl.signal);
        sugg = r.filter((s) => s.id !== exclude).slice(0, 8);
        active = sugg.length && trimmed ? 0 : -1;
      } catch {
        /* aborted */
      }
    }, trimmed ? 90 : 0);
  }

  function onInput() {
    open = true;
    fetchNames();
  }

  function pick(s?: Name) {
    if (s) onpick({ subject: s.id, name: s.name });
    else if (trimmed) onpick(exact ? { subject: exact.id, name: exact.name } : { name: trimmed });
    else return;
    open = false;
  }

  function onKey(e: KeyboardEvent) {
    const n = sugg.length + (trimmed && !exact ? 1 : 0);
    if (e.key === 'ArrowDown') {
      open = true;
      active = Math.min(n - 1, active + 1);
      e.preventDefault();
    } else if (e.key === 'ArrowUp') {
      active = Math.max(-1, active - 1);
      e.preventDefault();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(active >= 0 && active < sugg.length ? sugg[active] : undefined);
    } else if (e.key === 'Escape') {
      e.stopPropagation();
      if (open && (sugg.length || trimmed)) open = false;
      else oncancel?.();
    }
    e.stopPropagation();
  }
</script>

<div class="ni" class:compact>
  <input
    bind:this={input}
    class="field"
    bind:value={text}
    oninput={onInput}
    onfocus={() => {
      open = true;
      fetchNames();
    }}
    onblur={() => setTimeout(() => (open = false), 150)}
    onkeydown={onKey}
    placeholder={placeholder ?? (isPet ? 'Name this pet' : 'Who is this?')}
    maxlength="80"
    autocomplete="off"
    spellcheck="false"
    aria-label={placeholder ?? 'Name'}
  />
  {#if open && (sugg.length || trimmed)}
    <div class="list" role="listbox">
      {#each sugg as s, i (s.id)}
        <button class="opt" class:active={i === active} role="option" aria-selected={i === active} onmousedown={(e) => e.preventDefault()} onclick={() => pick(s)}>
          <Icon name={s.kind === 'person' ? 'user' : 'paw'} size={14} />
          <span class="grow ellipsis">{s.name}</span>
          <span class="dim tnum">{s.photos.toLocaleString()}</span>
        </button>
      {/each}
      {#if trimmed && !exact}
        <button class="opt new" class:active={active === sugg.length} onmousedown={(e) => e.preventDefault()} onclick={() => pick()}>
          <Icon name="plus" size={14} />
          <span class="grow ellipsis">{isPet ? 'New pet' : 'New person'}: <b>{trimmed}</b></span>
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .ni {
    position: relative;
    width: 100%;
  }
  .ni .field {
    width: 100%;
    height: 2.25rem;
  }
  .compact .field {
    height: 1.875rem;
    font-size: var(--fs-sm);
  }
  .list {
    position: absolute;
    z-index: 90;
    top: calc(100% + 0.25rem);
    left: 0;
    right: 0;
    min-width: 12rem;
    background: var(--raised);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-2);
    padding: 0.25rem;
    max-height: 18rem;
    overflow: auto;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.4375rem 0.5rem;
    border: 0;
    border-radius: var(--r);
    background: none;
    text-align: left;
    cursor: pointer;
    color: var(--text);
  }
  .opt :global(svg) {
    color: var(--text-3);
    flex: none;
  }
  .opt.active,
  .opt:hover {
    background: var(--surface-2);
  }
  .opt.new b {
    font-weight: 600;
  }
</style>
