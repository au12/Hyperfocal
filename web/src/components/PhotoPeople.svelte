<script lang="ts">
  import { untrack } from 'svelte';
  import { urls, type Detection, type PhotoPeople } from '../lib/api';
  import { kindWord, notWord, people, unknownLabel } from '../lib/people.svelte';
  import { store } from '../lib/store.svelte';
  import DetMenu from './DetMenu.svelte';
  import Icon from './Icon.svelte';
  import NameInput from './NameInput.svelte';

  /** Who is in a photo: names, suggestions to confirm, unknown faces to name, and the
   * corrections (not a face; a dog or a cat taken for a face). */
  let { photo }: { photo: number } = $props();

  let data = $state<PhotoPeople | null>(null);
  let naming = $state<number | null>(null);
  let menu = $state<{ det: Detection; at: { x: number; y: number; w: number; h: number }; anchor: HTMLElement | null } | null>(null);
  let showIgnored = $state(false);
  let shown = -1;

  async function load(id: number) {
    try {
      const d = await people.photo(id);
      if (id === photo) data = d;
    } catch {
      if (id === photo) data = null;
    }
  }

  $effect(() => {
    const id = photo;
    void people.rev;
    untrack(() => {
      if (id !== shown) {
        shown = id;
        data = null;
        naming = null;
        menu = null;
        showIgnored = false;
        load(id);
      } else if (people.affects(id)) load(id);
    });
  });

  const dets = $derived((data?.detections ?? []).filter((d) => !d.who?.hidden));
  const hiddenCount = $derived((data?.detections ?? []).length - dets.length);
  const ignored = $derived(data?.ignored_list ?? []);
  const ignoredText = $derived.by(() => {
    const n = ignored.length;
    if (!n) return '';
    const faces = ignored.every((d) => d.kind === 'face');
    const animals = ignored.every((d) => d.kind !== 'face');
    return faces || animals ? `${n} marked as ${notWord(faces ? 'face' : 'dog', n)}` : `${n} marked as wrong detections`;
  });

  function name(d: Detection, p: { subject?: number; name: string }) {
    naming = null;
    people.assign([d.id], p.subject != null ? { subject: p.subject } : { name: p.name }, p.name);
  }

  function show(d: Detection) {
    if (!d.who?.name) return;
    store.loupe = false;
    store.showSubject(d.who.id, d.who.name);
  }

  function openMenu(d: Detection, e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (menu?.det.id === d.id) {
      menu = null;
      return;
    }
    const btn = e.type === 'contextmenu' ? null : (e.currentTarget as HTMLElement);
    const r = btn?.getBoundingClientRect();
    menu = { det: d, at: r ? { x: r.left, y: r.top, w: r.width, h: r.height } : { x: e.clientX, y: e.clientY, w: 0, h: 0 }, anchor: btn };
  }

  /** The line under a name: what it is and how sure. */
  function meta(d: Detection): string {
    const parts: string[] = [];
    if (d.kind !== 'face') parts.push(kindWord(d.kind) === 'cat' ? 'Cat' : 'Dog');
    if (d.who?.name && d.state === 1) parts.push(d.src === 1 ? 'From the sidecar' : 'Confirmed');
    else if (d.who?.name) parts.push(d.sim != null ? `Suggested · ${Math.round(d.sim * 100)}% alike` : 'Suggested');
    else if (d.who) parts.push('In an unnamed group');
    else parts.push(d.kind === 'face' && d.q < 0.1 ? 'Not named · a poor view' : 'Not named');
    return parts.join(' · ');
  }

  const leave = (d: Detection) => {
    if (people.hover === d.id) people.hover = null;
  };
</script>

{#if data}
  {#if !data.analyzed}
    <div class="note">
      {#if data.error}
        <Icon name="info" size={14} /><span>Not analysed: {data.error}</span>
      {:else if people.status?.status.enabled && people.status.status.phase === 'preparing'}
        <span class="spinner sm"></span><span>Waiting for the AI models…</span>
      {:else if people.status?.status.enabled}
        <span class="spinner sm"></span><span>Looking for faces and pets…</span>
      {:else}
        <span>Not analysed</span>
      {/if}
    </div>
  {/if}
  {#if dets.length}
    <ul class="list">
      {#each dets as d (d.id)}
        {@const who = d.who}
        {@const sug = !!who?.name && d.state === 0}
        <li
          class="row"
          class:hl={people.hover === d.id || menu?.det.id === d.id}
          onpointerenter={() => (people.hover = d.id)}
          onpointerleave={() => leave(d)}
          oncontextmenu={(e) => openMenu(d, e)}
        >
          <span class="av" class:pet={d.kind !== 'face'} class:named={!!who?.name && d.state === 1} class:sug>
            <img src={urls.crop(d)} alt="" loading="lazy" draggable="false" />
            {#if d.kind !== 'face'}<span class="kind" title={kindWord(d.kind)}><Icon name={d.kind === 'cat' ? 'cat' : 'dog'} size={10} stroke={2.2} /></span>{/if}
          </span>
          {#if naming === d.id}
            <div class="grow namer"><NameInput kind={d.kind} compact onpick={(p) => name(d, p)} oncancel={() => (naming = null)} /></div>
          {:else}
            <div class="txt">
              {#if who?.name}
                <button class="nm ellipsis" class:sug onclick={() => show(d)} title={`All photos of ${who.name}`}>{who.name}{sug ? '?' : ''}</button>
              {:else}
                <span class="nm unk ellipsis">{unknownLabel(d)}</span>
              {/if}
              <span class="meta ellipsis">{meta(d)}</span>
            </div>
            <div class="acts">
              {#if sug}
                <button class="ib ok" onclick={() => people.confirm({ ids: [d.id] })} title={`Yes, ${who?.name}`} aria-label={`Yes, ${who?.name}`}><Icon name="check" size={15} stroke={2.2} /></button>
                <button class="ib no" onclick={() => people.reject([d.id])} title={`Not ${who?.name}`} aria-label={`Not ${who?.name}`}><Icon name="x" size={15} stroke={2.2} /></button>
              {:else if !who?.name}
                <button class="btn sm namebtn" onclick={() => (naming = d.id)}><Icon name="plus" />Name</button>
              {/if}
              <button class="ib more" class:on={menu?.det.id === d.id} onclick={(e) => openMenu(d, e)} title="More" aria-label="More" aria-haspopup="menu"><Icon name="more" size={16} /></button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if data.analyzed}
    <div class="empty">No faces or pets found{hiddenCount ? ` (${hiddenCount} hidden)` : ''}.</div>
  {/if}
  {#if ignored.length}
    <button class="ign-toggle" onclick={() => (showIgnored = !showIgnored)} aria-expanded={showIgnored}>
      <Icon name="chevron" size={13} class={showIgnored ? 'open' : ''} />{ignoredText}
    </button>
    {#if showIgnored}
      <ul class="list ignored">
        {#each ignored as d (d.id)}
          <li class="row" class:hl={people.hover === d.id} onpointerenter={() => (people.hover = d.id)} onpointerleave={() => leave(d)}>
            <span class="av" class:pet={d.kind !== 'face'}><img src={urls.crop(d)} alt="" loading="lazy" draggable="false" /></span>
            <div class="txt">
              <span class="nm unk ellipsis">{d.kind === 'face' ? 'Not a face' : 'Not a dog or cat'}</span>
              <span class="meta">You said so</span>
            </div>
            <div class="acts">
              <button class="btn sm ghost" onclick={() => people.restore([d.id])} title={`It is a ${kindWord(d.kind)} after all`}><Icon name="restore" />Restore</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
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
  .list {
    list-style: none;
    margin: 0 -0.5rem;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    min-height: 3rem;
    padding: 0.3125rem 0.375rem 0.3125rem 0.5rem;
    border-radius: var(--r-lg);
    transition: background-color var(--dur-fast);
  }
  .row.hl {
    background: var(--surface-2);
  }
  .av {
    position: relative;
    flex: none;
    width: 2.375rem;
    height: 2.375rem;
    border-radius: 50%;
    background: var(--surface-3);
    box-shadow: 0 0 0 1px var(--border);
  }
  .av.pet {
    border-radius: 0.625rem;
  }
  .av img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: inherit;
  }
  .av.named {
    box-shadow:
      0 0 0 2px var(--surface),
      0 0 0 3.5px var(--accent);
  }
  .av.sug {
    box-shadow:
      0 0 0 2px var(--surface),
      0 0 0 3.5px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .kind {
    position: absolute;
    right: -0.25rem;
    bottom: -0.25rem;
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    color: var(--text-2);
    box-shadow: 0 0 0 2px var(--surface);
  }
  .txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.0625rem;
  }
  .nm {
    border: 0;
    background: none;
    padding: 0;
    text-align: left;
    font-weight: 600;
    color: var(--text);
    cursor: pointer;
    max-width: 100%;
  }
  button.nm:hover {
    color: var(--accent);
  }
  .nm.sug {
    color: var(--text-2);
  }
  .nm.unk {
    color: var(--text-2);
    font-weight: 550;
    cursor: default;
  }
  .meta {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    flex: none;
  }
  .ib {
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    background: var(--surface-2);
    color: var(--text-2);
    display: grid;
    place-items: center;
    cursor: pointer;
    transition:
      color var(--dur-fast),
      background-color var(--dur-fast),
      border-color var(--dur-fast),
      transform var(--dur-fast) var(--ease-out);
  }
  .ib:active {
    transform: scale(0.92);
  }
  .ib.ok:hover {
    color: var(--positive);
    border-color: color-mix(in srgb, var(--positive) 50%, transparent);
    background: color-mix(in srgb, var(--positive) 12%, transparent);
  }
  .ib.no:hover {
    color: var(--negative);
    border-color: color-mix(in srgb, var(--negative) 50%, transparent);
    background: color-mix(in srgb, var(--negative) 12%, transparent);
  }
  .ib.more {
    border-color: transparent;
    background: none;
  }
  .ib.more:hover,
  .ib.more.on {
    color: var(--text);
    background: var(--surface-3);
  }
  .namebtn {
    height: 1.625rem;
    padding: 0 0.5rem 0 0.375rem;
    gap: 0.25rem;
  }
  .namer {
    min-width: 0;
  }
  .note,
  .empty {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .spinner.sm {
    width: 0.875rem;
    height: 0.875rem;
  }
  .ign-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    align-self: flex-start;
    border: 0;
    background: none;
    padding: 0.125rem 0;
    font-size: var(--fs-sm);
    color: var(--text-3);
    cursor: pointer;
  }
  .ign-toggle:hover {
    color: var(--text-2);
  }
  .ign-toggle :global(svg) {
    transition: transform var(--dur) var(--ease-out);
  }
  .ign-toggle :global(svg.open) {
    transform: rotate(90deg);
  }
  .ignored .av {
    opacity: 0.55;
    filter: grayscale(0.6);
  }
  @media (hover: hover) {
    .row:not(.hl) .ib.more {
      opacity: 0.55;
    }
  }
</style>
