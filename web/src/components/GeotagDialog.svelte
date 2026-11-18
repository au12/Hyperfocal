<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import type * as Leaflet from 'leaflet';
  import { store } from '../lib/store.svelte';
  import { api, urls, type GeotagParams, type GeotagPreview, type TrackInfo } from '../lib/api';
  import Icon from './Icon.svelte';

  // The photos: the selection, else everything in the view (as it was on opening).
  const fromSelection = store.selected.size > 0;
  const ids = fromSelection ? [...store.selected] : store.list.id.slice();

  let el: HTMLDivElement | undefined = $state();
  let fileInput: HTMLInputElement | undefined = $state();
  let L: typeof Leaflet;
  let map: Leaflet.Map | null = null;
  let layer: Leaflet.LayerGroup | null = null;
  let fitted = '';

  let uploads = $state<{ key: number; name: string; progress: number; reading: boolean }[]>([]);
  let uploadSeq = 0;
  let errors = $state<string[]>([]);
  let dragging = $state(false);

  let tzChoice = $state<'auto' | number>('auto');
  let driftText = $state('');
  let gapMin = $state(30);
  let replace = $state(false);

  let pv = $state.raw<GeotagPreview | null>(null);
  let loading = $state(false);
  let pvError = $state('');
  let applying = $state(false);

  const ro = $derived(!!store.status?.readonly);

  // The browser's zone at the time of the photos: the likeliest camera setting.
  const prefer = (() => {
    const t = ids.map((id) => store.list.t[store.index.get(id) ?? -1]).filter((v): v is number => v != null).sort((a, b) => a - b);
    const mid = t.length ? t[t.length >> 1] * 1000 : Date.now();
    return -new Date(mid).getTimezoneOffset();
  })();

  const ZONES = Array.from({ length: (840 + 720) / 15 + 1 }, (_, i) => -720 + i * 15);
  const zoneName = (m: number) => {
    const a = Math.abs(m);
    return `UTC${m < 0 ? '−' : '+'}${String(Math.floor(a / 60)).padStart(2, '0')}:${String(a % 60).padStart(2, '0')}`;
  };

  /** "+2:00", "-1:30", "1:02:03", "90" (seconds) -> seconds; null when unreadable. */
  function parseDrift(s: string): number | null {
    const t = s.trim().replace('−', '-');
    if (!t) return 0;
    const m = t.match(/^([+-])?\s*(?:(\d+):)?(\d+)(?::(\d+))?\s*s?$/);
    if (!m) return null;
    const sign = m[1] === '-' ? -1 : 1;
    let secs: number;
    if (m[4] != null) secs = (+(m[2] ?? 0)) * 3600 + +m[3] * 60 + +m[4];
    else if (m[2] != null) secs = +m[2] * 60 + +m[3];
    else secs = +m[3];
    return Math.abs(secs) > 7 * 86400 ? null : sign * secs;
  }
  const drift = $derived(parseDrift(driftText));
  const fmtDrift = (s: number) => {
    const a = Math.abs(s);
    const h = Math.floor(a / 3600);
    const m = Math.floor((a % 3600) / 60);
    const sec = a % 60;
    return `${s < 0 ? '−' : '+'}${h ? `${h}:${String(m).padStart(2, '0')}` : m}:${String(sec).padStart(2, '0')}`;
  };

  const params = $derived<GeotagParams | null>(
    store.tracks.length && ids.length && drift != null
      ? { tracks: store.tracks.map((t) => t.id), ids, tz: tzChoice === 'auto' ? null : tzChoice, drift, gap: gapMin * 60, replace, prefer }
      : null,
  );

  // Preview on every change (debounced; the newest request wins).
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctl: AbortController | null = null;
  $effect(() => {
    const p = params;
    clearTimeout(timer);
    ctl?.abort();
    if (!p) {
      pv = null;
      return;
    }
    timer = setTimeout(() => untrack(() => runPreview(p)), 200);
  });

  async function runPreview(p: GeotagParams) {
    const c = new AbortController();
    ctl = c;
    loading = true;
    try {
      const r = await api.geotagPreview(p, c.signal);
      if (c.signal.aborted) return;
      pv = r;
      pvError = '';
      draw(r);
    } catch (e) {
      if ((e as Error).name === 'AbortError') return;
      const err = e as Error & { status?: number };
      if (err.status === 410) {
        // The server let the tracks go (after a few hours unused): they must be added again.
        store.tracks = [];
        errors = ['The track files were unloaded after a few hours unused: add them again.'];
      } else pvError = err.message;
    } finally {
      if (ctl === c) loading = false;
    }
  }

  onMount(() => {
    let dead = false;
    (async () => {
      L = (await import('leaflet')).default;
      if (dead || !el) return;
      map = L.map(el, { worldCopyJump: true, preferCanvas: true }).setView([30, 0], 2);
      L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
        maxZoom: 19,
        attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
      }).addTo(map);
      layer = L.layerGroup().addTo(map);
      setTimeout(() => map?.invalidateSize(), 60);
      if (pv) draw(pv);
    })();
    return () => {
      dead = true;
      clearTimeout(timer);
      ctl?.abort();
      map?.remove();
      map = null;
    };
  });

  function draw(r: GeotagPreview) {
    if (!map || !layer || !L) return;
    layer.clearLayers();
    const accent = getComputedStyle(document.documentElement).getPropertyValue('--accent-strong').trim() || '#0070f2';
    for (const line of r.track) {
      if (line.length > 1) L.polyline(line, { color: accent, weight: 3, opacity: 0.85, interactive: false }).addTo(layer);
      else if (line.length === 1) L.circleMarker(line[0], { radius: 3, color: accent, weight: 2, interactive: false }).addTo(layer);
    }
    // Every photo up to a few thousand (a sample beyond), each with its thumbnail.
    const step = Math.max(1, Math.ceil(r.photos.length / 4000));
    for (let i = 0; i < r.photos.length; i += step) {
      const [id, lat, lon] = r.photos[i];
      const m = L.circleMarker([lat, lon], { radius: 5, color: '#fff', weight: 1.5, fillColor: '#f28c00', fillOpacity: 1 });
      m.on('click', () => {
        const at = store.index.get(id);
        const name = at != null ? store.list.n[at] : '';
        const img = at != null ? `<img src="${urls.thumb(id, store.list.v[at])}" alt="" style="display:block;max-width:180px;max-height:180px;border-radius:4px">` : '';
        m.bindPopup(`${img}<div style="margin-top:4px;font-size:12px">${name.replace(/[<&]/g, (c) => (c === '<' ? '&lt;' : '&amp;'))}</div>`).openPopup();
      });
      m.addTo(layer);
    }
    // Frame the result when the tracks change (not on every tweak of the settings).
    const key = store.tracks.map((t) => t.id).join(',');
    if (key !== fitted) {
      const pts: [number, number][] = r.photos.length ? r.photos.map((p) => [p[1], p[2]]) : r.track.flat();
      if (pts.length) {
        map.fitBounds(L.latLngBounds(pts), { padding: [24, 24], maxZoom: 16 });
        fitted = key;
      }
    }
  }

  async function addFiles(list: FileList | File[] | null) {
    const files = [...(list ?? [])];
    for (const f of files) {
      const key = ++uploadSeq;
      uploads = [...uploads, { key, name: f.name, progress: 0, reading: false }];
      const entry = () => uploads.find((u) => u.key === key);
      try {
        const t = await api.uploadTrack(f, (x) => {
          const u = entry();
          if (!u) return;
          u.progress = x;
          u.reading = x >= 1;
        });
        store.tracks = [...store.tracks, t];
        errors = errors.filter((e) => !e.startsWith(`${f.name}:`));
      } catch (e) {
        errors = [...errors.filter((x) => !x.startsWith(`${f.name}:`)), `${f.name}: ${(e as Error).message}`];
      } finally {
        uploads = uploads.filter((u) => u.key !== key);
      }
    }
    if (fileInput) fileInput.value = '';
  }

  function removeTrack(t: TrackInfo) {
    store.tracks = store.tracks.filter((x) => x.id !== t.id);
    api.forgetTrack(t.id).catch(() => {});
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    addFiles(e.dataTransfer?.files ?? null);
  }

  async function apply() {
    if (!params || !pv?.matched) return;
    applying = true;
    try {
      const r = await api.geotagApply(params);
      store.onJob(r.job);
      store.dialog = null;
    } catch (e) {
      store.toast(`Not saved: ${(e as Error).message}`, 'error', undefined, 8000);
    } finally {
      applying = false;
    }
  }

  const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  const span = (a: number, b: number) => {
    const da = new Date(a).toDateString() === new Date(b).toDateString();
    return da ? `${when(a)} – ${new Date(b).toLocaleTimeString(undefined, { timeStyle: 'short' })}` : `${when(a)} – ${when(b)}`;
  };
  const plural = (n: number, w: string) => `${n.toLocaleString()} ${w}${n === 1 ? '' : 's'}`;
  /** Every photo with a time records its zone: the camera zone does not matter. */
  const allOwn = $derived(!!pv && pv.own_zone > 0 && pv.own_zone + pv.no_time >= pv.total);
  const wide = $derived(!!pv?.plateau && pv.plateau[1] - pv.plateau[0] >= 30 && !allOwn);
</script>

<!-- Files can be dropped anywhere (never opened by the browser in place of the app). -->
<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (store.dialog = null)} ondragover={(e) => e.preventDefault()} ondrop={onDrop}>
  <div class="dialog geo" role="dialog" aria-modal="true" aria-label="Geotag from a track">
    <header>
      <Icon name="route" />
      <h2>Geotag from a track</h2>
      <button class="btn ghost icon" onclick={() => (store.dialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="cols">
      <div class="side">
        <section>
          <div class="lab">Track files</div>
          {#each store.tracks as t (t.id)}
            <div class="track">
              <Icon name="route" size={16} />
              <div class="grow">
                <div class="ellipsis" title={t.name}>{t.name}</div>
                <div class="dim small">{t.format} · {plural(t.points, 'point')} · {span(t.start, t.end)}</div>
              </div>
              <button class="btn sm ghost icon" onclick={() => removeTrack(t)} aria-label="Remove {t.name}"><Icon name="x" /></button>
            </div>
          {/each}
          {#each uploads as u}
            <div class="track">
              <span class="spinner"></span>
              <div class="grow">
                <div class="ellipsis">{u.name}</div>
                <div class="dim small">{u.reading ? 'Reading…' : `Uploading ${Math.round(u.progress * 100)}%`}</div>
              </div>
            </div>
          {/each}
          <button class="drop" class:on={dragging} onclick={() => fileInput?.click()} ondragenter={() => (dragging = true)} ondragleave={(e) => { if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node)) dragging = false; }}>
            <Icon name="upload" />
            <span>{store.tracks.length ? 'Add another file' : 'Choose or drop track files'}</span>
            <span class="dim small">GPX, KML, TCX, NMEA, Google Timeline or Takeout location history (JSON)</span>
          </button>
          <!-- No accept filter: phones grey out .gpx files they have no type for. -->
          <input bind:this={fileInput} type="file" multiple hidden onchange={(e) => addFiles((e.target as HTMLInputElement).files)} />
          {#each errors as e}<div class="err small">{e}</div>{/each}
        </section>

        <section>
          <div class="lab">Photos</div>
          <div>{ids.length ? `${fromSelection ? 'The' : 'All'} ${plural(ids.length, fromSelection ? 'selected photo' : 'photo')}${fromSelection ? '' : ' in this view'}` : 'No photos in this view'}</div>
        </section>

        <section>
          <label class="lab" for="geo-tz">Camera time zone</label>
          <select id="geo-tz" class="field" value={String(tzChoice)} disabled={allOwn} onchange={(e) => { const v = (e.target as HTMLSelectElement).value; tzChoice = v === 'auto' ? 'auto' : +v; }}>
            <option value="auto">Find it{pv?.detected ? ` (${zoneName(pv.tz)})` : ''}</option>
            {#each ZONES as z}<option value={String(z)}>{zoneName(z)}{z === prefer ? ' · this device' : ''}</option>{/each}
          </select>
          {#if pv?.own_zone}<div class="dim small">{plural(pv.own_zone, 'photo')} {pv.own_zone === 1 ? 'records its' : 'record their'} own time zone, which is used for {pv.own_zone === 1 ? 'it' : 'them'}.</div>{/if}
          {#if tzChoice === 'auto' && wide && pv?.plateau}
            <div class="note small"><Icon name="info" size={14} />Zones from {zoneName(pv.plateau[0])} to {zoneName(pv.plateau[1])} fit as many photos: check a few on the map, or choose the zone the camera was set to.</div>
          {/if}
          <label class="lab" for="geo-drift">Camera clock correction</label>
          <input id="geo-drift" class="field" class:bad={drift == null} bind:value={driftText} placeholder="+0:00 (minutes:seconds)" autocomplete="off" />
          <div class="dim small">{drift == null ? 'Write it as minutes:seconds, e.g. −2:30, or seconds.' : drift ? `${fmtDrift(drift)} added to the camera's times` : 'Use when the camera clock was off: −2:00 if it was two minutes fast.'}</div>
        </section>

        <section>
          <label class="lab" for="geo-gap">Longest gap in the track to bridge</label>
          <select id="geo-gap" class="field" bind:value={gapMin}>
            {#each [1, 5, 15, 30, 60, 120, 360] as m}<option value={m}>{m < 60 ? `${m} min` : `${m / 60} h`}</option>{/each}
          </select>
          <label class="check small"><input type="checkbox" bind:checked={replace} /> Also photos that already have a location</label>
        </section>
      </div>
      <div class="mapwrap">
        <div class="mapbox" bind:this={el}></div>
        {#if loading}<div class="busy"><span class="spinner"></span></div>{/if}
      </div>
    </div>
    <footer>
      <div class="grow summary small">
        {#if !store.tracks.length}
          <span class="dim">Add a GPS track or a phone's location history to place the photos on it.</span>
        {:else if pvError}
          <span class="err">{pvError}</span>
        {:else if pv}
          <span><b class="tnum">{pv.matched.toLocaleString()}</b> of {plural(pv.total, 'photo')} fall on the track</span>
          <span class="dim">
            {[pv.outside ? `${pv.outside.toLocaleString()} outside its time` : '', pv.no_time ? `${pv.no_time.toLocaleString()} without a capture time` : '', pv.has_gps ? `${pv.has_gps.toLocaleString()} already located (kept)` : ''].filter(Boolean).join(' · ')}
          </span>
          {#if !pv.matched && pv.span}
            <span class="dim">The photos were taken {span(pv.span[0], pv.span[1])}; the track runs {span(pv.range[0], pv.range[1])} (times on this device).</span>
          {/if}
        {/if}
      </div>
      <button class="btn" onclick={() => (store.dialog = null)}>Cancel</button>
      <button class="btn primary" onclick={apply} disabled={ro || applying || loading || !pv?.matched || !params} title={ro ? 'Read-only mode' : ''}>
        {#if applying}<span class="spinner"></span>{/if}Write {pv?.matched ? plural(pv.matched, 'location') : 'locations'}
      </button>
    </footer>
  </div>
</div>

<style>
  .dialog.geo {
    width: min(72rem, 100%);
    height: min(88vh, 50rem);
  }
  .cols {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .side {
    width: 23rem;
    flex: none;
    overflow: auto;
    border-right: 1px solid var(--border);
    padding: 0.25rem 1.25rem 1rem;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 0.4375rem;
    padding: 0.875rem 0;
    border-bottom: 1px solid var(--border);
  }
  section:last-child {
    border-bottom: 0;
  }
  .lab {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-3);
  }
  section .lab:not(:first-child) {
    margin-top: 0.375rem;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .err {
    color: var(--negative);
  }
  .track {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    padding: 0.375rem 0;
  }
  .track .spinner {
    width: 1rem;
    height: 1rem;
    flex: none;
  }
  .drop {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    padding: 0.875rem;
    border: 1.5px dashed var(--border-strong);
    border-radius: var(--r-lg);
    background: none;
    color: var(--text);
    cursor: pointer;
    text-align: center;
  }
  .drop:hover,
  .drop.on {
    border-color: var(--accent);
    background: var(--accent-bg);
  }
  .field.bad {
    border-color: var(--negative);
  }
  .note {
    display: flex;
    gap: 0.375rem;
    align-items: flex-start;
    color: var(--critical);
  }
  .note :global(svg) {
    flex: none;
    margin-top: 0.125rem;
  }
  .mapwrap {
    flex: 1;
    min-width: 0;
    position: relative;
  }
  .mapbox {
    position: absolute;
    inset: 0;
  }
  .busy {
    position: absolute;
    top: 0.75rem;
    right: 0.75rem;
    z-index: 500;
    background: var(--raised);
    border-radius: 50%;
    padding: 0.375rem;
    box-shadow: var(--shadow-1);
    display: flex;
  }
  :global([data-theme='dark']) .mapbox :global(.leaflet-tile-pane) {
    filter: brightness(0.8) contrast(1.1);
  }
  footer {
    align-items: center;
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
  }
  @media (max-width: 760px) {
    .dialog.geo {
      height: 100%;
      max-height: 100%;
      border-radius: 0;
      padding-top: env(safe-area-inset-top);
    }
    :global(.scrim:has(.dialog.geo)) {
      padding: 0;
    }
    .cols {
      flex-direction: column;
      overflow: auto;
    }
    .side {
      width: auto;
      overflow: visible;
      border-right: 0;
    }
    .mapwrap {
      order: -1;
      flex: none;
      height: 38vh;
    }
    footer {
      flex-wrap: wrap;
      padding-bottom: calc(1rem + env(safe-area-inset-bottom));
    }
    .summary {
      flex-basis: 100%;
    }
  }
</style>
