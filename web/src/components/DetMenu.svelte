<script lang="ts">
  // What can be said about one face or animal: who it is (or is not), and corrections of
  // the detectors (not a face; a dog or a cat taken for a face). Opens from a ⋯ button or
  // a right-click, over everything (it lives in <body>, so a scrolling panel or a tile's
  // rounded corners never clip it).
  import { onMount } from 'svelte';
  import type { Detection } from '../lib/api';
  import { kindWord, people } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';

  let {
    det,
    who,
    at,
    anchor = null,
    showAll = true,
    onname,
    onopen,
    onclose,
  }: {
    det: Detection;
    /** Who it is said (or suggested) to be: its person, pet or unnamed group. */
    who: { id: number; name: string | null } | null;
    /** Where to open (viewport px): under this rectangle, or at a point (w = h = 0). */
    at: { x: number; y: number; w: number; h: number };
    /** The button that opened it (a click on it closes the menu, not reopens it). */
    anchor?: HTMLElement | null;
    /** Offer "All photos of …" (not in the person's own sheet). */
    showAll?: boolean;
    /** Name it (or name someone else): the host shows its name field. */
    onname?: () => void;
    /** Open the photo it is in. */
    onopen?: () => void;
    onclose: () => void;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let pos = $state<{ left: number; top: number; up: boolean } | null>(null);

  const face = $derived(det.kind === 'face');
  const name = $derived(who?.name ?? null);
  const suggested = $derived(!!name && det.state === 0);
  const what = $derived(kindWord(det.kind));

  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  onMount(() => {
    if (el) {
      const r = el.getBoundingClientRect();
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      // Under the anchor, its right edges lined up (a ⋯ button sits at the right).
      let left = at.w ? at.x + at.w - r.width : at.x;
      left = Math.max(8, Math.min(left, vw - r.width - 8));
      let top = at.y + at.h + 6;
      let up = false;
      if (top + r.height > vh - 8 && at.y - r.height - 6 >= 8) {
        top = at.y - r.height - 6;
        up = true;
      }
      top = Math.max(8, Math.min(top, vh - r.height - 8));
      pos = { left, top, up };
    }
    const outside = (e: Event) => {
      const t = e.target as Node;
      if (el?.contains(t) || anchor?.contains(t)) return;
      onclose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onclose();
      }
    };
    const away = () => onclose();
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('keydown', key, true);
    window.addEventListener('resize', away);
    window.addEventListener('wheel', away, { passive: true });
    return () => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('resize', away);
      window.removeEventListener('wheel', away);
    };
  });

  /** Do it, then close (closing lets go of the detection these props come from). */
  function run(f: () => unknown) {
    f();
    onclose();
  }
</script>

<div
  bind:this={el}
  use:portal
  class="pop detmenu"
  class:up={pos?.up}
  role="menu"
  tabindex="-1"
  style:left={`${pos?.left ?? 0}px`}
  style:top={`${pos?.top ?? 0}px`}
  style:visibility={pos ? 'visible' : 'hidden'}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#if suggested}
    <button class="menu-item yes" role="menuitem" onclick={() => run(() => people.confirm({ ids: [det.id] }))}><Icon name="check" />Yes, it’s {name}</button>
  {/if}
  {#if name}
    <button class="menu-item" role="menuitem" onclick={() => run(() => people.reject([det.id]))}><Icon name="user-x" />Not {name}</button>
  {:else if who}
    <button class="menu-item" role="menuitem" onclick={() => run(() => people.reject([det.id]))}><Icon name="user-x" />Not in this group</button>
  {/if}
  {#if onname}
    <button class="menu-item" role="menuitem" onclick={() => run(onname)}>
      <Icon name={name ? 'users' : 'edit'} />{name ? (face ? 'Someone else…' : 'Another pet…') : face ? 'Name this person…' : `Name this ${what}…`}
    </button>
  {/if}
  {#if name && who && showAll}
    <button class="menu-item" role="menuitem" onclick={() => run(() => { store.loupe = false; people.open = null; store.showSubject(who.id, name); })}><Icon name="grid" />All photos of {name}</button>
  {/if}
  {#if onopen}
    <button class="menu-item" role="menuitem" onclick={() => run(onopen)}><Icon name="image" />Open the photo</button>
  {/if}
  <div class="menu-sep"></div>
  <div class="menu-label">Wrong detection?</div>
  {#if face}
    <button class="menu-item" role="menuitem" onclick={() => run(() => people.setKind([det.id], 'dog'))}><Icon name="dog" />It’s a dog</button>
    <button class="menu-item" role="menuitem" onclick={() => run(() => people.setKind([det.id], 'cat'))}><Icon name="cat" />It’s a cat</button>
    <button class="menu-item warn" role="menuitem" onclick={() => run(() => people.notDetected([det.id], det.kind))}><Icon name="face-off" />Not a face</button>
  {:else}
    {#if det.fixed}
      <button class="menu-item" role="menuitem" onclick={() => run(() => people.setKind([det.id], det.kind === 'dog' ? 'cat' : 'dog'))}>
        <Icon name={det.kind === 'dog' ? 'cat' : 'dog'} />It’s a {det.kind === 'dog' ? 'cat' : 'dog'}
      </button>
      <button class="menu-item" role="menuitem" onclick={() => run(() => people.setKind([det.id], 'face'))}><Icon name="scan-face" />It’s a face after all</button>
    {/if}
    <button class="menu-item warn" role="menuitem" onclick={() => run(() => people.notDetected([det.id], det.kind))}><Icon name="ban" />Not a dog or cat</button>
  {/if}
</div>

<style>
  .detmenu {
    position: fixed;
    z-index: 120;
    width: max-content;
    min-width: 13.5rem;
    max-width: min(19rem, calc(100vw - 1rem));
    transform-origin: top right;
  }
  .detmenu.up {
    transform-origin: bottom right;
  }
  .menu-item {
    white-space: nowrap;
  }
  .yes :global(svg) {
    color: var(--positive);
  }
  .warn,
  .warn :global(svg) {
    color: var(--negative);
  }
  .warn:hover {
    background: color-mix(in srgb, var(--negative) 12%, transparent);
  }
  .menu-label {
    padding-top: 0.25rem;
  }
</style>
