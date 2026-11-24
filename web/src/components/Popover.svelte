<script lang="ts">
  import type { Snippet } from 'svelte';
  import { tick } from 'svelte';
  import { pop, slide, fade } from '../lib/motion';
  import { store } from '../lib/store.svelte';

  let {
    trigger,
    children,
    align = 'left',
    open = $bindable(false),
    width,
    up = false,
    sheet = false,
    label = '',
    flush = false,
  }: {
    trigger: Snippet<[() => void, boolean]>;
    children: Snippet<[() => void]>;
    align?: 'left' | 'right';
    open?: boolean;
    width?: string;
    up?: boolean;
    /** On a phone, open as a sheet from the bottom of the screen (long content). */
    sheet?: boolean;
    /** The sheet's title (phones). */
    label?: string;
    /** No padding: the content draws its own (a panel with a header and a footer). */
    flush?: boolean;
  } = $props();

  let root: HTMLElement | undefined = $state();
  let box: HTMLElement | undefined = $state();
  /** Room on screen: the popover never runs past the window's edges. */
  let maxH = $state<number | null>(null);
  let shift = $state(0);
  let upNow = $state(false);
  const asSheet = $derived(sheet && store.mobile);

  function toggle() {
    open = !open;
  }
  function close() {
    open = false;
  }

  /** Opens where there is room (below, else above), as tall as the room allows (the
   * content scrolls), and moved sideways to stay inside the window. */
  function place() {
    if (!box || !root || asSheet) return;
    const M = 8;
    const vh = window.innerHeight;
    const vw = window.innerWidth;
    const r = root.getBoundingClientRect();
    const natural = box.scrollHeight;
    const below = vh - r.bottom - M - 6;
    const above = r.top - M - 6;
    let u = up;
    if (!up && natural > below && above > below * 1.25) u = true;
    if (up && natural > above && below > above * 1.25) u = false;
    upNow = u;
    maxH = Math.max(140, Math.floor(u ? above : below));
    shift = 0;
    const b = box.getBoundingClientRect();
    if (b.right > vw - M) shift = vw - M - b.right;
    if (b.left + shift < M) shift = M - b.left;
  }

  $effect(() => {
    if (!open) return;
    maxH = null;
    shift = 0;
    upNow = up;
    tick().then(place);
    const onDown = (e: PointerEvent) => {
      if (root && !root.contains(e.target as Node)) open = false;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        open = false;
        e.stopPropagation();
      }
    };
    const onResize = () => place();
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('resize', onResize);
    // Content that grows (a list loading in) gets its room again.
    const ro = new ResizeObserver(() => place());
    if (box) ro.observe(box.firstElementChild ?? box);
    return () => {
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('resize', onResize);
      ro.disconnect();
    };
  });
</script>

<div class="popwrap" bind:this={root}>
  {@render trigger(toggle, open)}
  {#if open}
    {#if asSheet}
      <div class="sheet-scrim" transition:fade={{ duration: 180 }} onclick={close} role="presentation"></div>
      <div class="sheet" role="dialog" aria-label={label || undefined} bind:this={box} transition:slide={{ duration: 260, y: 100 }}>
        <div class="grab" aria-hidden="true"></div>
        {#if label && !flush}
          <div class="sheet-head">
            <span class="grow">{label}</span>
            <button class="btn ghost sm" onclick={close}>Done</button>
          </div>
        {/if}
        <div class="sheet-body" class:flush>{@render children(close)}</div>
      </div>
    {:else}
      <div
        class="pop"
        class:right={align === 'right'}
        class:up={upNow}
        class:flush
        bind:this={box}
        style:width
        style:max-height={maxH != null ? `${maxH}px` : undefined}
        style:margin-left={shift ? `${shift}px` : undefined}
        style:animation="none"
        transition:pop={{ duration: 170, y: upNow ? 4 : -4, origin: `${upNow ? 'bottom' : 'top'} ${align === 'right' ? 'right' : 'left'}` }}
      >
        {@render children(close)}
      </div>
    {/if}
  {/if}
</div>

<style>
  .popwrap {
    position: relative;
    display: inline-flex;
  }
  .pop {
    top: calc(100% + 0.375rem);
    left: 0;
    max-width: calc(100vw - 1rem);
    overflow-x: hidden;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .pop.flush {
    padding: 0;
  }
  .sheet-body.flush {
    padding: 0;
  }
  .pop.right {
    left: auto;
    right: 0;
  }
  /* Opening upwards (from a bar at the bottom of the screen, or short of room below). */
  .pop.up {
    top: auto;
    bottom: calc(100% + 0.5rem);
  }
  .sheet-scrim {
    position: fixed;
    inset: 0;
    z-index: 89;
    background: var(--scrim);
  }
  .sheet {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 90;
    max-height: min(86dvh, 46rem);
    display: flex;
    flex-direction: column;
    background: var(--raised);
    border-radius: var(--r-2xl) var(--r-2xl) 0 0;
    box-shadow: var(--shadow-2);
    padding-bottom: env(safe-area-inset-bottom);
  }
  .grab {
    width: 2.25rem;
    height: 0.25rem;
    border-radius: 2px;
    background: var(--border-strong);
    margin: 0.5rem auto 0.25rem;
    flex: none;
  }
  .sheet-head {
    display: flex;
    align-items: center;
    padding: 0.25rem 0.75rem 0.375rem 1.125rem;
    font-size: var(--fs-lg);
    font-weight: 680;
    flex: none;
  }
  .sheet-body {
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 0 0.5rem 0.75rem;
    min-height: 0;
  }
</style>
