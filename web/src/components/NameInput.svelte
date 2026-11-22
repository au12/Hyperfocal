<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api, urls, type DetKind, type PersonName } from '../lib/api';
  import Icon from './Icon.svelte';

  /** A name for a face or an animal: an existing person or pet (suggested as you type,
   * most photographed first), or a new one. */
  let {
    kind,
    value = '',
    placeholder,
    exclude,
    autofocus = true,
    listOnFocus = true,
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
    /** Show the names as soon as the field has the focus (else on a click or typing). */
    listOnFocus?: boolean;
    compact?: boolean;
    onpick: (p: { subject?: number; name: string }) => void;
    oncancel?: () => void;
  } = $props();

  type Name = PersonName;
  // The starting text; typing takes over from there.
  let text = $state(untrack(() => value));
  let sugg = $state<Name[]>([]);
  let active = $state(-1);
  let open = $state(false);
  let input: HTMLInputElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctl: AbortController | null = null;
  // Without `listOnFocus`, the list opens when you click into the field or type, not when
  // the field gets the focus on opening (where it would cover the faces being named).
  let pressed = false;

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
    onpointerdown={() => (pressed = true)}
    onfocus={() => {
      fetchNames();
      if (pressed || listOnFocus) open = true;
      pressed = false;
    }}
    onclick={() => {
      if (!open) {
        open = true;
        fetchNames();
      }
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
          <span class="av" class:pet={s.kind !== 'person'}>
            {#if s.cover}<img src={urls.crop(s.cover)} alt="" loading="lazy" draggable="false" />{:else}<Icon name={s.kind === 'person' ? 'user' : 'paw'} size={13} />{/if}
          </span>
          <span class="grow ellipsis">{s.name}</span>
          <span class="n tnum">{s.photos.toLocaleString()}</span>
        </button>
      {/each}
      {#if trimmed && !exact}
        <button class="opt new" class:active={active === sugg.length} onmousedown={(e) => e.preventDefault()} onclick={() => pick()}>
          <span class="av plus"><Icon name="plus" size={14} stroke={2.2} /></span>
          <span class="grow ellipsis">{isPet ? 'New pet' : 'New person'}: <b>{trimmed}</b></span>
          <kbd>↵</kbd>
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
    gap: 0.5625rem;
    width: 100%;
    padding: 0.3125rem 0.5rem 0.3125rem 0.375rem;
    border: 0;
    border-radius: var(--r);
    background: none;
    text-align: left;
    cursor: pointer;
    color: var(--text);
    font-size: var(--fs);
  }
  .opt.active,
  .opt:hover {
    background: var(--surface-3);
  }
  .av {
    flex: none;
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    color: var(--text-3);
    box-shadow: 0 0 0 1px var(--border);
  }
  .av.pet {
    border-radius: 0.5rem;
  }
  .av img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .av.plus {
    background: var(--accent-bg-strong);
    color: var(--accent);
    box-shadow: none;
  }
  .n {
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .opt.new b {
    font-weight: 650;
  }
  .opt kbd {
    opacity: 0;
  }
  .opt.new.active kbd {
    opacity: 1;
  }
</style>
