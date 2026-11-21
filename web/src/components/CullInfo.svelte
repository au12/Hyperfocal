<script lang="ts">
  // What culling measured in a photo: the sharpness of its subject, the eyes, the
  // exposure, and its place in a series (with a way to keep the best frame).
  import { cull, focusTone, focusWord, issueOf } from '../lib/cull.svelte';
  import { store } from '../lib/store.svelte';
  import type { CullPhoto } from '../lib/api';
  import Icon from './Icon.svelte';

  let { photo }: { photo: number } = $props();

  let data = $state<CullPhoto | null>(null);
  $effect(() => {
    const id = photo;
    void cull.rev;
    let live = true;
    cull.get(id).then((c) => {
      if (live) data = c && c.id === id ? c : null;
    });
    return () => {
      live = false;
    };
  });

  const ro = $derived(!!store.status?.readonly);
  const on = $derived(data?.focus_on === 'faces' ? 'on the faces' : data?.focus_on === 'animals' ? 'on the animal' : data?.focus_on === 'photo' ? 'sharpest part' : '');
  const f = $derived(data?.focus ?? null);
  // The ring's arc: a circle of radius 15.9 has a circumference of about 100.
  const dash = $derived(`${Math.round((f ?? 0) * 100)} 100`);
  const pct = (v: number | null | undefined, digits = 1) => (v == null ? '–' : `${(v * 100).toFixed(v * 100 < 10 ? digits : 0)}%`);
  const series = $derived(data?.series ?? null);
  const at = $derived(series ? series.frames.findIndex((fr) => fr.id === photo) : -1);
  const bestAt = $derived(series ? series.frames.findIndex((fr) => fr.id === series.best) : -1);
</script>

{#if data}
  <section class="cull">
    <div class="lab row">
      <span class="grow">Culling</span>
      {#if data.pending}<span class="measuring" title="Measured in the background: the photos on screen first"><span class="spinner"></span>Measuring</span>{/if}
    </div>
    {#if data.error}
      <div class="dim small">Could not be measured: {data.error}</div>
    {:else if data.measured}
      <div class="focus {focusTone(f)}">
        <svg viewBox="0 0 36 36" class="ring" aria-hidden="true">
          <circle cx="18" cy="18" r="15.9" class="track" />
          <circle cx="18" cy="18" r="15.9" class="arc" stroke-dasharray={dash} />
        </svg>
        <div class="ftext">
          <div class="big">{focusWord(f)} <span class="tnum dim">{f == null ? '' : `${Math.round(f * 100)}`}</span></div>
          <div class="dim small">{on ? `Sharpness ${on}` : 'Sharpness'}, from the {store.list.k[store.index.get(photo) ?? -1] === 1 ? 'RAW' : 'photo'}</div>
        </div>
      </div>
      <div class="facts">
        {#if data.eyes}
          <div class="fact" class:bad={(data.eyes.closed ?? 0) > 0}>
            <Icon name={(data.eyes.closed ?? 0) > 0 ? 'eye-closed' : 'eye'} size={15} />
            {#if data.eyes.checked === 0}
              <span class="dim">No face close enough to check the eyes</span>
            {:else if (data.eyes.closed ?? 0) > 0}
              <span>Eyes closed: {data.eyes.closed} of {data.eyes.checked} {data.eyes.checked === 1 ? 'face' : 'faces'}</span>
            {:else}
              <span>Eyes open{data.eyes.checked > 1 ? ` (${data.eyes.checked} faces)` : ''}</span>
            {/if}
          </div>
        {/if}
        <div class="fact" class:bad={data.issues.includes('over')}>
          <Icon name="sun" size={15} />
          <span>Clipped <span class="tnum">{pct(data.over)}</span></span>
        </div>
        <div class="fact" class:bad={data.issues.includes('under')}>
          <Icon name="moon" size={15} />
          <span>Deep shadows <span class="tnum">{pct(data.under, 0)}</span></span>
        </div>
      </div>
      {#if data.issues.length}
        <div class="row wrap issues">
          {#each data.issues as k (k)}
            {@const is = issueOf(k)}
            <span class="issue {k}" title={is.hint}><Icon name={is.icon} size={12} />{is.label}</span>
          {/each}
        </div>
      {/if}
    {:else}
      <div class="dim small">Not measured yet.</div>
    {/if}
    {#if series}
      <div class="series">
        <div class="srow">
          <Icon name="stack" size={15} />
          <span>Frame <b class="tnum">{at + 1}</b> of {series.frames.length} in a series</span>
        </div>
        <div class="frames" style:--n={series.frames.length}>
          {#each series.frames as fr, k (fr.id)}
            <button
              class="fr"
              class:cur={k === at}
              class:isbest={fr.id === series.best}
              class:poor={fr.issues.includes('blur') || fr.issues.includes('eyes')}
              onclick={() => cull.showFrame(fr.id, series.key)}
              title={`${fr.name}${fr.id === series.best ? ' · best' : ''}${fr.issues.length ? ' · ' + fr.issues.map((x) => issueOf(x).label.toLowerCase()).join(', ') : ''}`}
            ></button>
          {/each}
        </div>
        <div class="dim small">
          {#if series.best === photo}
            The best of the series{#if series.why}: {series.why.toLowerCase()}{/if}.
          {:else}
            Best: frame {bestAt + 1}{#if series.why}{' '}({series.why.toLowerCase()}){/if}.
          {/if}
        </div>
        <div class="row gap">
          {#if series.best !== photo}
            <button class="btn sm" onclick={() => cull.showFrame(series.best, series.key)}><Icon name="arrow-up-right" />Go to best</button>
          {/if}
          {#if !ro}
            <button class="btn sm keep" onclick={() => data && cull.keepBest(data)} title={'Move the other frames to the reject folder (undoable)' + store.keyHint('keepBest')}><Icon name="check" />Keep best</button>
          {/if}
        </div>
      </div>
    {/if}
  </section>
{/if}

<style>
  .cull {
    animation: fade-in var(--dur) var(--ease-out);
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
  }
  .measuring {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    color: var(--text-3);
    font-size: var(--fs-xs, 0.6875rem);
    font-weight: 600;
    text-transform: none;
    letter-spacing: 0;
  }
  .focus {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 0.25rem 0 0.625rem;
  }
  .ring {
    width: 2.75rem;
    height: 2.75rem;
    flex: none;
    transform: rotate(-90deg);
  }
  .ring circle {
    fill: none;
    stroke-width: 3.4;
  }
  .ring .track {
    stroke: var(--surface-3);
  }
  .ring .arc {
    stroke: var(--text-3);
    stroke-linecap: round;
    transition: stroke-dasharray 0.6s var(--ease-out);
    animation: arc-in 0.8s var(--ease-out);
  }
  @keyframes arc-in {
    from {
      stroke-dasharray: 0 100;
    }
  }
  .good .arc {
    stroke: var(--positive);
  }
  .fair .arc {
    stroke: #f5b301;
  }
  .poor .arc {
    stroke: var(--negative);
  }
  .big {
    font-size: 1rem;
    font-weight: 680;
    letter-spacing: -0.01em;
  }
  .facts {
    display: grid;
    gap: 0.3125rem;
  }
  .fact {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .fact.bad {
    color: var(--text);
  }
  .fact.bad :global(svg) {
    color: var(--negative);
  }
  .issues {
    margin-top: 0.5rem;
    gap: 0.3125rem;
  }
  .issue {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    height: 1.375rem;
    padding: 0 0.5rem;
    border-radius: 99px;
    font-size: 0.6875rem;
    font-weight: 650;
    background: color-mix(in srgb, var(--c) 16%, transparent);
    color: var(--c);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--c) 35%, transparent);
  }
  .issue.blur {
    --c: #f59e0b;
  }
  .issue.eyes {
    --c: #ff5c61;
  }
  .issue.over {
    --c: #fb7a3c;
  }
  .issue.under {
    --c: #6b8cff;
  }
  .series {
    margin-top: 0.75rem;
    padding-top: 0.625rem;
    border-top: 1px solid var(--border);
    display: grid;
    gap: 0.4375rem;
  }
  .srow {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--fs-sm);
  }
  .frames {
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    gap: 3px;
    max-width: 16rem;
  }
  .fr {
    height: 0.5rem;
    border: 0;
    padding: 0;
    border-radius: 2px;
    background: var(--surface-3);
    cursor: pointer;
    transition:
      background var(--dur-fast),
      transform var(--dur-fast);
  }
  .fr:hover {
    transform: scaleY(1.4);
  }
  .fr.poor {
    background: color-mix(in srgb, var(--negative) 45%, var(--surface-3));
  }
  .fr.isbest {
    background: #f5b301;
  }
  .fr.cur {
    box-shadow: 0 0 0 2px var(--accent);
  }
  .keep {
    color: var(--positive);
    border-color: color-mix(in srgb, var(--positive) 40%, transparent);
  }
  .keep:hover {
    background: color-mix(in srgb, var(--positive) 12%, transparent);
  }
  .gap {
    gap: 0.375rem;
  }
</style>
