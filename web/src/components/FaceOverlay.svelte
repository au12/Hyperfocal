<script lang="ts">
  import { untrack } from 'svelte';
  import type { Detection, PhotoPeople } from '../lib/api';
  import { people } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import Icon from './Icon.svelte';
  import NameInput from './NameInput.svelte';

  /** The faces and pets of the photo in the single view, with their names, drawn over
   * the image. `rect`: where the image is on screen (CSS px, relative to the stage). */
  let { photo, rect }: { photo: number; rect: { x: number; y: number; w: number; h: number } | null } = $props();

  let data = $state<PhotoPeople | null>(null);
  let naming = $state<number | null>(null);
  let shown = -1;

  $effect(() => {
    const id = photo;
    void people.rev;
    untrack(async () => {
      if (id !== shown) {
        shown = id;
        data = null;
        naming = null;
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

  function label(d: Detection): string {
    const n = d.who?.name;
    if (n) return d.state === 1 ? n : `${n}?`;
    return '';
  }

  function name(d: Detection, p: { subject?: number; name: string }) {
    naming = null;
    people.assign([d.id], p.subject != null ? { subject: p.subject } : { name: p.name }, p.name);
  }
</script>

{#if rect && dets.length}
  <div class="ov" style:left={`${rect.x}px`} style:top={`${rect.y}px`} style:width={`${rect.w}px`} style:height={`${rect.h}px`}>
    {#each dets as d (d.id)}
      {@const [x, y, w, h] = d.box}
      {@const small = w * rect.w < 34}
      <div class="box" class:named={d.state === 1 && d.who?.name} class:sug={d.state === 0 && d.who?.name} class:pet={d.kind !== 'face'} class:small style:left={`${x * 100}%`} style:top={`${y * 100}%`} style:width={`${w * 100}%`} style:height={`${h * 100}%`}>
        {#if naming === d.id}
          <div class="namer">
            <NameInput kind={d.kind} compact onpick={(p) => name(d, p)} oncancel={() => (naming = null)} />
          </div>
        {:else if label(d)}
          <div class="tag">
            {#if d.state === 0 && d.who?.name}
              <button class="lbl" onclick={() => (naming = d.id)} title="Someone else">{label(d)}</button>
              <button class="yn" onclick={() => people.confirm({ ids: [d.id] })} title={`Yes, ${d.who?.name}`} aria-label="Yes"><Icon name="check" size={12} /></button>
              <button class="yn" onclick={() => people.reject([d.id])} title={`Not ${d.who?.name}`} aria-label="No"><Icon name="x" size={12} /></button>
            {:else}
              <button class="lbl" onclick={() => (naming = d.id)} title="Change">{label(d)}</button>
            {/if}
          </div>
        {:else if w * rect.w >= 14}
          <!-- Unknown: a small button, so crowded photos stay readable. -->
          <div class="tag"><button class="yn add" onclick={() => (naming = d.id)} title={d.kind === 'face' ? 'Who is this?' : 'Name this pet'} aria-label="Name"><Icon name="plus" size={12} /></button></div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .ov {
    position: absolute;
    pointer-events: none;
    z-index: 2;
  }
  .box {
    position: absolute;
    border: 2px solid rgba(255, 255, 255, 0.85);
    border-radius: 0.375rem;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.45), inset 0 0 0 1px rgba(0, 0, 0, 0.25);
  }
  .box.named {
    border-color: #4db1ff;
  }
  .box.sug {
    border-style: dashed;
  }
  .box.pet {
    border-radius: 0.75rem;
  }
  .tag,
  .namer {
    position: absolute;
    top: calc(100% + 0.25rem);
    left: 50%;
    transform: translateX(-50%);
    pointer-events: auto;
    white-space: nowrap;
  }
  .namer {
    width: 13rem;
  }
  .tag {
    display: flex;
    gap: 0.125rem;
    align-items: center;
  }
  .lbl,
  .yn {
    border: 0;
    border-radius: 1rem;
    background: rgba(14, 18, 22, 0.78);
    color: #fff;
    font-size: 0.75rem;
    font-weight: 600;
    height: 1.375rem;
    padding: 0 0.5rem;
    cursor: pointer;
    backdrop-filter: blur(4px);
  }
  .yn {
    width: 1.375rem;
    padding: 0;
    display: grid;
    place-items: center;
  }
  .lbl:hover,
  .yn:hover {
    background: rgba(0, 112, 242, 0.9);
  }
  .small .tag .lbl {
    font-size: 0.6875rem;
    height: 1.25rem;
  }
  .box:has(.tag:hover),
  .box:has(.namer) {
    z-index: 1;
  }
  .yn.add {
    width: 1.125rem;
    height: 1.125rem;
    opacity: 0.85;
  }
</style>
