<script lang="ts">
  // Settings → Debug (only with HYPERFOCAL_DEBUG on the server): what the telemetry sees
  // now, the report to download, a moment to mark, and memory to hand back.
  import { onMount } from 'svelte';
  import { api, urls } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { tel, MARK_KEY } from '../lib/telemetry';
  import { prettyKey } from '../lib/keys';
  import Icon from './Icon.svelte';

  const markShortcut = prettyKey(MARK_KEY);

  let live = $state<Record<string, any> | null>(null);
  let err = $state('');
  let me = $state(tel.snapshot());
  let days = $state(3);
  let note = $state('');
  let busy = $state('');
  let summary = $state<string | null>(null);
  let summaryEl = $state<HTMLTextAreaElement | null>(null);

  async function refresh() {
    try {
      live = await api.debugLive();
      err = '';
    } catch (e) {
      err = (e as Error).message;
    }
    me = tel.snapshot();
  }

  onMount(() => {
    refresh();
    const t = setInterval(() => {
      if (!document.hidden) refresh();
    }, 5000);
    return () => clearInterval(t);
  });

  async function mark() {
    busy = 'mark';
    const ok = await tel.mark(note.trim());
    busy = '';
    if (ok) {
      store.toast('Moment marked in the telemetry', 'ok');
      note = '';
      refresh();
    } else store.toast('Could not mark the moment', 'error');
  }

  async function trim() {
    busy = 'trim';
    try {
      const r = await api.debugTrim();
      store.toast(`Released ${r.freed_mb} MB to the system (RSS ${r.rss_before_mb} → ${r.rss_after_mb} MB, ${r.ms} ms)`, 'ok', undefined, 8000);
      refresh();
    } catch (e) {
      store.toast(`Not released: ${(e as Error).message}`, 'error');
    }
    busy = '';
  }

  async function copySummary() {
    busy = 'summary';
    try {
      const text = await api.debugSummary();
      // The clipboard needs a secure connection (HTTPS or localhost): else the text is
      // shown, selected, to copy by hand.
      if (window.isSecureContext && navigator.clipboard) {
        await navigator.clipboard.writeText(text);
        store.toast('Summary copied: paste it into the conversation', 'ok');
        summary = null;
      } else {
        summary = text;
        queueMicrotask(() => summaryEl?.select());
      }
    } catch (e) {
      store.toast(`No summary: ${(e as Error).message}`, 'error');
    }
    busy = '';
  }

  const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? '' : 's'}`;
  const f1 = (x: unknown) => (typeof x === 'number' ? (Math.round(x * 10) / 10).toLocaleString() : '–');
  const size = (mb: unknown) => (typeof mb !== 'number' ? '–' : mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`);
  const ms = (x: unknown) => (typeof x !== 'number' ? '–' : x >= 1000 ? `${(x / 1000).toFixed(1)} s` : `${Math.round(x)} ms`);
  const ago = (iso: string) => {
    const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
    return s < 90 ? `${Math.round(s)} s ago` : s < 5400 ? `${Math.round(s / 60)} min ago` : `${Math.round(s / 3600)} h ago`;
  };
  const shortUa = (ua: string) => {
    const b = ua.match(/(Edg|Firefox|Chrome|Safari)\/[\d.]+/)?.[1] ?? 'Browser';
    const os = /iPhone|iPad/.test(ua) ? 'iOS' : /Android/.test(ua) ? 'Android' : /Mac OS X/.test(ua) ? 'macOS' : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : '';
    return `${b === 'Edg' ? 'Edge' : b}${os ? ' on ' + os : ''}`;
  };

  const s = $derived(live?.latest ?? null);
  const dbg = $derived(store.status?.debug ?? null);
  const busiest = $derived(((s?.threads ?? []) as any[]).filter((t) => t.cpu >= 1).slice(0, 4));
  const kids = $derived((s?.children ?? []) as any[]);
  const reqs = $derived(Object.values((s?.http ?? {}) as Record<string, any>).reduce((a: number, r: any) => a + (r.n ?? 0), 0));
  const others = $derived(((live?.clients ?? []) as any[]).filter((c) => c.dev !== tel.dev));
</script>

<div class="dbg">
  <p class="lede dim">
    Debug mode is on (<code>HYPERFOCAL_DEBUG</code>): Hyperfocal records errors, timings, memory and CPU use, and what this browser sees, to
    <code>AppData/debug</code>{#if dbg}{' '}— {dbg.dir_mb} of {dbg.max_mb} MB, kept {dbg.days} days, a sample every {dbg.interval} s{/if}. Remove the variable to turn it off.
  </p>

  <section>
    <h4>Report</h4>
    <p class="blurb dim">One file with a summary, the full data and the telemetry files, to hand over for analysis. Passwords, tokens and API keys are left out; folder and file names are in it.</p>
    <div class="row wrap">
      <select class="field small" bind:value={days} aria-label="Days of telemetry">
        {#each [[1, 'Last day'], [3, 'Last 3 days'], [7, 'Last week'], [30, 'Last 30 days']] as [d, label]}<option value={d}>{label}</option>{/each}
      </select>
      <a class="btn primary" href={urls.debugBundle(days)} download><Icon name="download" />Download report</a>
      <button class="btn" onclick={copySummary} disabled={busy === 'summary'}><Icon name="copy" />Copy summary</button>
    </div>
    {#if summary != null}
      <textarea class="field summary" readonly bind:this={summaryEl} value={summary}></textarea>
      <p class="hint dim">Copying needs HTTPS: select all and copy it by hand.</p>
    {/if}
  </section>

  <section>
    <h4>Mark a moment</h4>
    <p class="blurb dim">
      Saw something slow or wrong? Say what, and it is noted with what was on screen, so it can be found in the telemetry.{#if !store.mobile}{' '}Anywhere
        in the app: {markShortcut}.{/if}
    </p>
    <form class="row" onsubmit={(e) => (e.preventDefault(), mark())}>
      <input class="field grow" placeholder="e.g. the RAW view took ages on this photo" bind:value={note} maxlength="1000" />
      <button class="btn" type="submit" disabled={busy === 'mark'}><Icon name="bookmark-plus" />Mark</button>
    </form>
  </section>

  <section>
    <h4>Server now</h4>
    {#if err}
      <p class="blurb neg">{err}</p>
    {:else if !s}
      <p class="blurb dim">The first sample comes {dbg ? `within ${dbg.interval} s of the start` : 'shortly'}.</p>
    {:else}
      <dl>
        <dt>CPU</dt>
        <dd class="tnum">{f1(s.proc?.cpu)}% of a core{#if busiest.length}{' '}· {busiest.map((t) => `${t.name} ${Math.round(t.cpu)}%`).join(' · ')}{/if}</dd>
        <dt>Memory</dt>
        <dd class="tnum">
          {size(s.proc?.rss_mb)} resident (own {size(s.proc?.anon_mb)}, mapped files {size(s.proc?.file_mb)}, peak {size(s.proc?.hwm_mb)})
          {#if s.malloc}<div class="dim">allocator: {size(s.malloc.in_use_mb)} in use, {size(s.malloc.free_mb)} freed but held</div>{/if}
        </dd>
        {#if s.cg}
          <dt>Container</dt>
          <dd class="tnum">{size(s.cg.mem_mb)}{s.cg.max_mb ? ` of ${size(s.cg.max_mb)}` : ' (no limit set)'}{#if s.cg.oom_kill}{' '}· <span class="neg">{s.cg.oom_kill} killed for memory</span>{/if}{#if s.cg.psi?.io}{' '}· I/O stalls {f1(s.cg.psi.io.some60)}%{/if}</dd>
        {/if}
        {#if kids.length}
          <dt>Helpers</dt>
          <dd class="tnum">{kids.map((k) => `${k.name} ${size(k.rss_mb)}, ${Math.round(k.cpu)}%`).join(' · ')}</dd>
        {/if}
        {#if s.gpu}
          <dt>GPU</dt>
          <dd class="tnum">{(s.gpu as any[]).map((g) => `${g.name}: ${g.util}% busy, ${g.mem_used_mb} of ${g.mem_total_mb} MB, ${g.temp} °C`).join(' · ')}</dd>
        {/if}
        <dt>Disk</dt>
        <dd class="tnum">reading {f1(s.proc?.read_mbs)} MB/s{#if s.disk?.config}{' '}· AppData {f1(s.disk.config.free_gb)} GB free{/if}</dd>
        <dt>Traffic</dt>
        <dd class="tnum">{plural(reqs, 'request')} in the last {Math.round(s.dt)} s · {plural(live?.http?.open_streams ?? 0, 'event stream')} open</dd>
        <dt>Problems</dt>
        <dd class="tnum">
          {live?.logs?.counts?.error ?? 0} errors · {live?.logs?.counts?.warn ?? 0} warnings{#if live?.panics?.length}{' '}· <span class="neg">{live.panics.length} panics</span>{/if}
          {#if live?.previous}<div class="neg">The previous run ended without shutting down cleanly.</div>{/if}
        </dd>
      </dl>
      <div class="row wrap">
        <button class="btn sm" onclick={trim} disabled={busy === 'trim'} title="Asks the allocator to give the memory it holds but does not use back to the system (malloc_trim), and says how much that was"><Icon name="refresh" />Release free memory</button>
        <button class="btn sm ghost" onclick={refresh}><Icon name="refresh" />Refresh</button>
      </div>
    {/if}
    {#if live?.logs?.sites?.length}
      <ul class="sites">
        {#each live.logs.sites.slice(0, 6) as x}
          <li><span class="lvl" class:neg={x.lvl === 'ERROR'}>{x.lvl.toLowerCase()} ×{x.n.toLocaleString()}</span> <span class="dim">{x.samples?.[x.samples.length - 1] ?? ''}</span></li>
        {/each}
      </ul>
    {/if}
  </section>

  <section>
    <h4>This browser</h4>
    <dl>
      <dt>Network</dt>
      <dd class="tnum">{plural(me.requests, 'request')}, {f1(me.mb)} MB · {plural(me.sse, 'event message')} ({me.sseKb} KB)</dd>
      <dt>Smoothness</dt>
      <dd class="tnum">{plural(me.long, 'long task')} ({me.longS} s){#if me.dropped != null}{' '}· {me.dropped}% of frames dropped while scrolling or panning{/if}</dd>
      <dt>Memory</dt>
      <dd class="tnum">
        {#if me.jsMb != null}JS {size(me.jsMb)}{' '}·{' '}{/if}images {size(me.bitmapsMb ?? 0)} · graphics ≈{size(me.gpuMb ?? 0)}
      </dd>
      {#if me.battery}
        <dt>Battery</dt>
        <dd class="tnum">{Math.round(me.battery.level * 100)}%{me.battery.charging ? ', charging' : ''}</dd>
      {/if}
      <dt>Errors</dt>
      <dd class="tnum" class:neg={me.errors > 0}>{me.errors}</dd>
    </dl>
  </section>

  {#if others.length}
    <section>
      <h4>Other browsers</h4>
      <ul class="sites">
        {#each others.slice(0, 6) as c}
          <li>
            {shortUa(c.env?.ua ?? '')}{c.env?.mobile ? ' (touch)' : ''}
            <span class="dim"
              >· last report {ago(c.last)} · {c.errors?.reduce((a: number, e: any) => a + e.n, 0) ?? 0} errors{#if c.timings?.['viewer.show.raw']}{' '}· RAW view p95 {ms(
                  c.timings['viewer.show.raw'].p95,
                )}{/if}{#if c.battery?.drain_pct_h}{' '}· battery −{c.battery.drain_pct_h}%/h{/if}</span
            >
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</div>

<style>
  .dbg {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    padding-bottom: 0.5rem;
  }
  .lede,
  .blurb {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .blurb {
    margin: 0.125rem 0 0.5rem;
  }
  h4 {
    margin: 0;
    font-size: var(--fs);
    font-weight: 650;
  }
  code {
    font-family: var(--mono);
    font-size: 0.95em;
  }
  .wrap {
    flex-wrap: wrap;
  }
  a.btn {
    text-decoration: none;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .field.small {
    width: auto;
    height: 2rem;
    font-size: var(--fs-sm);
  }
  .summary {
    width: 100%;
    height: 10rem;
    margin-top: 0.5rem;
    font-family: var(--mono);
    font-size: var(--fs-xs);
    white-space: pre;
  }
  .hint {
    margin: 0.25rem 0 0;
    font-size: var(--fs-xs);
  }
  dl {
    display: grid;
    grid-template-columns: 6rem 1fr;
    gap: 0.375rem 1rem;
    margin: 0 0 0.75rem;
    font-size: var(--fs-sm);
  }
  dt {
    color: var(--text-3);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .neg {
    color: var(--negative);
  }
  .sites {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: var(--fs-sm);
  }
  .sites li {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lvl {
    font-weight: 600;
  }
  @media (max-width: 760px) {
    dl {
      grid-template-columns: 5rem 1fr;
    }
  }
</style>
