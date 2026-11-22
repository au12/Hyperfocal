<script lang="ts">
  import { untrack } from 'svelte';
  import type { Detection, PhotoPeople } from '../lib/api';
  import { people } from '../lib/people.svelte';
  import DetMenu from './DetMenu.svelte';
  import Icon from './Icon.svelte';
  import NameInput from './NameInput.svelte';

  /** The faces and pets of the photo in the single view, with their names, drawn over
   * the image. `rect`: where the image is on screen (CSS px, relative to the stage). */
  let { photo, rect }: { photo: number; rect: { x: number; y: number; w: number; h: number } | null } = $props();

  let data = $state<PhotoPeople | null>(null);
  let naming = $state<number | null>(null);
  let menu = $state<{ det: Detection; at: { x: number; y: number; w: number; h: number }; anchor: HTMLElement | null } | null>(null);
  let shown = -1;

  $effect(() => {
    const id = photo;
    void people.rev;
    untrack(async () => {
      if (id !== shown) {
        shown = id;
        data = null;
        naming = null;
        menu = null;
      } else if (!people.affects(id)) return;
      try {
        const d = await people.photo(id);
        if (id === photo) data = d;
      } catch {
        /* not analysed or gone */
      }
    });
  });

  const dets = $derived((data?.detections ?? []).filter((d) => !d.who?.hidden));
  /** A detection marked "not a face", shown while its row in the info panel is pointed at. */
  const ghost = $derived((data?.ignored_list ?? []).find((d) => d.id === people.hover) ?? null);

  function name(d: Detection, p: { subject?: number; name: string }) {
    naming = null;
    people.assign([d.id], p.subject != null ? { subject: p.subject } : { name: p.name }, p.name);
  }

  function openMenu(d: Detection, e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (menu?.det.id === d.id) {
      menu = null;
      return;
    }
    const el = e.type === 'contextmenu' ? null : (e.currentTarget as HTMLElement);
    const r = el?.getBoundingClientRect();
    menu = { det: d, at: r ? { x: r.left, y: r.top, w: r.width, h: r.height } : { x: e.clientX, y: e.clientY, w: 0, h: 0 }, anchor: el };
  }

  const enter = (d: Detection) => (people.hover = d.id);
  const leave = (d: Detection) => {
    if (people.hover === d.id) people.hover = null;
  };
</script>

{#if rect && (dets.length || ghost)}
  <div class="ov" style:left={`${rect.x}px`} style:top={`${rect.y}px`} style:width={`${rect.w}px`} style:height={`${rect.h}px`}>
    {#if ghost}
      {@const [x, y, w, h] = ghost.box}
      <div class="box ghost hl" class:pet={ghost.kind !== 'face'} style:left={`${x * 100}%`} style:top={`${y * 100}%`} style:width={`${w * 100}%`} style:height={`${h * 100}%`}>
        <div class="tag" class:above={y + h > 0.92}><span class="pill ghost-pill">{ghost.kind === 'face' ? 'Not a face' : 'Not a dog or cat'}</span></div>
      </div>
    {/if}
    {#each dets as d (d.id)}
      {@const [x, y, w, h] = d.box}
      {@const who = d.who}
      {@const named = d.state === 1 && !!who?.name}
      {@const sug = d.state === 0 && !!who?.name}
      {@const px = w * rect.w}
      <div
        class="box"
        class:named
        class:sug
        class:pet={d.kind !== 'face'}
        class:hl={people.hover === d.id || menu?.det.id === d.id || naming === d.id}
        class:open={naming === d.id}
        style:left={`${x * 100}%`}
        style:top={`${y * 100}%`}
        style:width={`${w * 100}%`}
        style:height={`${h * 100}%`}
      >
        {#if naming === d.id}
          <div class="namer" class:above={y + h > 0.9}>
            <NameInput kind={d.kind} compact onpick={(p) => name(d, p)} oncancel={() => (naming = null)} />
          </div>
        {:else if px >= 14}
          <!-- One pill per face: the name (click: everything about it), a suggestion to answer,
               or a way to name it. Small faces in a crowd get a small pill. -->
          <div
            class="tag"
            class:above={y + h > 0.92}
            class:small={px < 40}
            role="group"
            onpointerenter={() => enter(d)}
            onpointerleave={() => leave(d)}
            oncontextmenu={(e) => openMenu(d, e)}
          >
            {#if named}
              <button class="pill name" onclick={(e) => openMenu(d, e)} title="Who it is, and corrections" aria-haspopup="menu">{who?.name}</button>
            {:else if sug}
              <button class="pill sug" onclick={(e) => openMenu(d, e)} title="More" aria-haspopup="menu">{who?.name}?</button>
              <button class="round ok" onclick={() => people.confirm({ ids: [d.id] })} title={`Yes, ${who?.name}`} aria-label="Yes"><Icon name="check" size={12} stroke={2.4} /></button>
              <button class="round no" onclick={() => people.reject([d.id])} title={`Not ${who?.name}`} aria-label="No"><Icon name="x" size={12} stroke={2.4} /></button>
            {:else}
              <button class="pill add" onclick={() => (naming = d.id)} title={d.kind === 'face' ? 'Who is this?' : 'Name this pet'}>
                <Icon name="plus" size={12} stroke={2.4} />{px < 40 ? '' : 'Name'}
              </button>
              <button class="round more" onclick={(e) => openMenu(d, e)} title="More (not a face, a dog or a cat…)" aria-label="More" aria-haspopup="menu"><Icon name="more" size={13} stroke={2.2} /></button>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

{#if menu}
  {@const m = menu}
  <DetMenu
    det={m.det}
    who={m.det.who ? { id: m.det.who.id, name: m.det.who.name } : null}
    at={m.at}
    anchor={m.anchor}
    onname={() => (naming = m.det.id)}
    onclose={() => (menu = null)}
  />
{/if}

<style>
  .ov {
    position: absolute;
    pointer-events: none;
    z-index: 2;
  }
  .box {
    --line: rgba(255, 255, 255, 0.82);
    position: absolute;
    border: 1.5px solid var(--line);
    border-radius: 0.5rem;
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.35),
      inset 0 0 0 1px rgba(0, 0, 0, 0.18);
    transition:
      border-color var(--dur-fast),
      box-shadow var(--dur),
      opacity var(--dur);
  }
  .box.pet {
    border-radius: 0.875rem;
  }
  .box.named {
    --line: #9aa6ff;
    border-width: 2px;
  }
  .box.sug {
    --line: rgba(154, 166, 255, 0.95);
    border-style: dashed;
  }
  .box.ghost {
    --line: rgba(255, 120, 120, 0.95);
    border-style: dashed;
  }
  .ghost-pill {
    cursor: default;
    color: #ffd0d0;
    box-shadow: inset 0 0 0 1px rgba(255, 120, 120, 0.5);
  }
  .box.hl {
    border-width: 2px;
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.4),
      0 0 0 5px color-mix(in srgb, var(--line) 22%, transparent),
      0 0 28px color-mix(in srgb, var(--line) 30%, transparent);
  }
  .box.hl,
  .box.open {
    z-index: 1;
  }
  /* One face in focus (its row in the info panel, its pill, its menu): the others step back. */
  .ov:has(.box.hl) .box:not(.hl) {
    opacity: 0.38;
  }
  .tag,
  .namer {
    position: absolute;
    top: calc(100% + 0.375rem);
    left: 50%;
    transform: translateX(-50%);
    pointer-events: auto;
    white-space: nowrap;
  }
  .tag.above,
  .namer.above {
    top: auto;
    bottom: calc(100% + 0.375rem);
  }
  .namer {
    width: 14rem;
  }
  .tag {
    display: flex;
    gap: 0.1875rem;
    align-items: center;
    filter: drop-shadow(0 4px 12px rgba(0, 0, 0, 0.35));
  }
  .pill,
  .round {
    height: 1.5rem;
    border: 0;
    border-radius: 999px;
    background: rgba(14, 15, 19, 0.8);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.12);
    backdrop-filter: blur(10px) saturate(1.4);
    -webkit-backdrop-filter: blur(10px) saturate(1.4);
    color: #fff;
    font: inherit;
    font-size: 0.75rem;
    font-weight: 600;
    letter-spacing: 0.005em;
    cursor: pointer;
    transition:
      background-color var(--dur-fast),
      color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out);
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0 0.625rem;
    max-width: 14rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pill.name {
    background: color-mix(in srgb, #6372ff 88%, transparent);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
  }
  .pill.name:hover {
    background: #6f7dff;
  }
  .pill.sug {
    color: #dfe3ff;
    box-shadow: inset 0 0 0 1px rgba(154, 166, 255, 0.55);
  }
  .pill.add {
    padding: 0 0.5625rem 0 0.4375rem;
    color: #eceef4;
  }
  .pill:hover,
  .round:hover {
    background: rgba(32, 34, 42, 0.92);
  }
  .round {
    width: 1.5rem;
    display: grid;
    place-items: center;
    padding: 0;
  }
  .round.ok:hover {
    background: #2a9d63;
  }
  .round.no:hover {
    background: #d64545;
  }
  .round.more {
    opacity: 0.85;
  }
  .pill:active,
  .round:active {
    transform: scale(0.94);
  }
  .tag.small .pill,
  .tag.small .round {
    height: 1.25rem;
    font-size: 0.6875rem;
  }
  .tag.small .round {
    width: 1.25rem;
  }
  .tag.small .pill.add {
    padding: 0 0.3125rem;
  }
</style>
