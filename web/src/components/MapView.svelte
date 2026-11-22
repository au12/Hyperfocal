<script lang="ts" module>
  /** Where the map was: coming back to Locations (from a photo, People or another
   * folder) puts it back there instead of zooming out to every photo again. */
  let saved: { lat: number; lng: number; zoom: number } | null = null;
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import type * as Leaflet from 'leaflet';
  import { store, LOCATIONS } from '../lib/store.svelte';
  import { api, urls, type Cluster } from '../lib/api';
  import { keysFor, prettyKey } from '../lib/keys';
  import Icon from './Icon.svelte';

  /** Markers (64 px, a single photo's 52, and the count's badge over the corner) closer
   * than this on both axes would overlap: they become one. */
  const MERGE_PX = 74;
  /** Web Mercator stops here: nothing to pan to beyond. */
  const MAX_LAT = 85.0511;

  let el: HTMLDivElement | undefined = $state();
  let L: typeof Leaflet;
  let map: Leaflet.Map | null = null;
  let layer: Leaflet.LayerGroup | null = null;
  let ctl: AbortController | null = null;
  let fitted = false;

  /** Nothing to show: no photo has a location yet, or none matches the search or filters. */
  const none = $derived(!store.loading && store.list.id.length === 0);
  const anyLocated = $derived((store.counts[LOCATIONS.id] ?? store.status?.counts.geotagged ?? 0) > 0);
  const locationKey = $derived(prettyKey(keysFor('location', store.settings.keymap, store.keyOverrides())[0] ?? 'Shift+L'));

  function scopeParams() {
    const p = store.params();
    delete (p as any).sort;
    delete (p as any).desc;
    return p;
  }

  /** Zooms out to every photo of Locations (with the search and filters); false when
   * there is none. */
  async function fitAll(animate = true): Promise<boolean> {
    if (!map) return false;
    const all = await api.clusters({ ...scopeParams(), zoom: 1 }).catch(() => null);
    if (!map || !all?.clusters.length) return false;
    let s = 90, w = 180, n = -90, e = -180;
    for (const c of all.clusters) {
      s = Math.min(s, c.bounds[0]);
      w = Math.min(w, c.bounds[1]);
      n = Math.max(n, c.bounds[2]);
      e = Math.max(e, c.bounds[3]);
    }
    map.fitBounds([[s, w], [n, e]], { padding: [48, 48], maxZoom: 13, animate });
    return true;
  }

  async function refresh() {
    if (!map) return;
    if (!fitted) {
      fitted = true;
      if (saved) map.setView([saved.lat, saved.lng], saved.zoom, { animate: false });
      else if (await fitAll(false)) return; // moveend refreshes
    }
    if (!map) return;
    ctl?.abort();
    const my = new AbortController();
    ctl = my;
    const b = map.getBounds();
    const z = map.getZoom();
    try {
      const r = await api.clusters(
        { ...scopeParams(), zoom: Math.round(z), west: b.getWest() - 1, east: b.getEast() + 1, south: b.getSouth() - 1, north: b.getNorth() + 1 },
        my.signal,
      );
      if (!my.signal.aborted) draw(merge(r.clusters));
    } catch {
      /* aborted */
    }
  }

  /** The server groups photos by grid cells of about a marker's size, so markers of
   * neighbouring cells can still sit on top of each other: those become one, where the
   * larger one was (with its photo), spanning both. Kept markers never move, so none of
   * them overlap. */
  function merge(cs: Cluster[]): Cluster[] {
    if (!map) return cs;
    const out: (Cluster & { x: number; y: number })[] = [];
    const cells = new Map<string, number[]>();
    const key = (x: number, y: number) => `${Math.floor(x / MERGE_PX)},${Math.floor(y / MERGE_PX)}`;
    for (const c of [...cs].sort((a, b) => b.n - a.n)) {
      const p = map.latLngToLayerPoint([c.lat, c.lon]);
      let into: (Cluster & { x: number; y: number }) | null = null;
      const gx = Math.floor(p.x / MERGE_PX);
      const gy = Math.floor(p.y / MERGE_PX);
      for (let dx = -1; dx <= 1 && !into; dx++) {
        for (let dy = -1; dy <= 1 && !into; dy++) {
          for (const k of cells.get(`${gx + dx},${gy + dy}`) ?? []) {
            const q = out[k];
            if (Math.abs(q.x - p.x) < MERGE_PX && Math.abs(q.y - p.y) < MERGE_PX) {
              into = q;
              break;
            }
          }
        }
      }
      if (into) {
        into.n += c.n;
        into.bounds = [Math.min(into.bounds[0], c.bounds[0]), Math.min(into.bounds[1], c.bounds[1]), Math.max(into.bounds[2], c.bounds[2]), Math.max(into.bounds[3], c.bounds[3])];
      } else {
        const k = key(p.x, p.y);
        cells.set(k, [...(cells.get(k) ?? []), out.length]);
        out.push({ ...c, bounds: [...c.bounds] as Cluster['bounds'], x: p.x, y: p.y });
      }
    }
    return out;
  }

  function draw(cs: Cluster[]) {
    if (!map || !layer) return;
    layer.clearLayers();
    const inView = map.getBounds();
    let n = 0;
    for (const c of cs) {
      if (inView.contains([c.lat, c.lon])) n += c.n;
      const size = c.n > 1 ? 64 : 52;
      const html = `<div class="mk${c.n > 1 ? ' many' : ''}"><img src="${c.v ? urls.thumb(c.id, c.v) : ''}" alt="" draggable="false" />${c.n > 1 ? `<span class="badge">${c.n > 999 ? Math.round(c.n / 1000) + 'k' : c.n}</span>` : ''}</div>`;
      const m = L.marker([c.lat, c.lon], {
        icon: L.divIcon({ html, className: 'hf-marker', iconSize: [size, size], iconAnchor: [size / 2, size / 2] }),
        riseOnHover: true,
        keyboard: true,
        title: c.n > 1 ? `${c.n.toLocaleString()} photos` : '',
      });
      m.on('click', () => openCluster(c));
      layer.addLayer(m);
    }
    store.mapInView = n;
  }

  /** A marker: zoom in to its photos, or open the photo (the best of a spot) in the
   * single view, over the map. */
  function openCluster(c: Cluster) {
    if (!map) return;
    const [s, w, n, e] = c.bounds;
    const spread = n - s > 1e-5 || e - w > 1e-5;
    if (c.n > 1 && spread && map.getZoom() < map.getMaxZoom() - 1) {
      map.fitBounds([[s, w], [n, e]], { padding: [80, 80], maxZoom: 18 });
      return;
    }
    const i = store.index.get(c.id);
    if (i != null) store.openLoupe(i);
  }

  /** The lowest zoom at which the world fills the map's height: no empty band above
   * the Arctic or below Antarctica. */
  function fillZoom(): number {
    const h = el?.clientHeight ?? 512;
    return Math.max(0, Math.ceil(Math.log2(Math.max(1, h) / 256)));
  }

  onMount(() => {
    let dead = false;
    let ro: ResizeObserver | null = null;
    (async () => {
      L = (await import('leaflet')).default;
      if (dead || !el) return;
      setup();
      // The sidebar and the window change the map's size: Leaflet needs to be told.
      let raf = 0;
      ro = new ResizeObserver(() => {
        cancelAnimationFrame(raf);
        raf = requestAnimationFrame(() => {
          if (!map) return;
          map.invalidateSize({ pan: false });
          map.setMinZoom(fillZoom());
        });
      });
      ro.observe(el);
    })();
    return () => {
      dead = true;
      ro?.disconnect();
      ctl?.abort();
      map?.remove();
      map = null;
      store.mapInView = null;
    };
  });

  function setup() {
    if (!el) return;
    map = L.map(el, {
      zoomControl: false,
      worldCopyJump: true,
      minZoom: fillZoom(),
      maxBounds: L.latLngBounds([-MAX_LAT, -1e5], [MAX_LAT, 1e5]),
      maxBoundsViscosity: 1,
    }).setView([30, -40], Math.max(2, fillZoom()));
    L.control.zoom({ position: 'topleft' }).addTo(map);
    // Back to every photo, under the zoom buttons.
    const Fit = L.Control.extend({
      onAdd: () => {
        const div = L.DomUtil.create('div', 'leaflet-bar leaflet-control');
        const a = L.DomUtil.create('a', 'hf-fit', div);
        a.href = '#';
        a.title = 'Show every photo';
        a.setAttribute('role', 'button');
        a.setAttribute('aria-label', 'Show every photo');
        a.innerHTML =
          '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3 M21 8V5a2 2 0 0 0-2-2h-3 M3 16v3a2 2 0 0 0 2 2h3 M16 21h3a2 2 0 0 0 2-2v-3"/></svg>';
        L.DomEvent.disableClickPropagation(div);
        L.DomEvent.on(a, 'click', (e) => {
          L.DomEvent.preventDefault(e);
          fitAll();
        });
        return div;
      },
    });
    new Fit({ position: 'topleft' }).addTo(map);
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', {
      maxZoom: 19,
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
    }).addTo(map);
    layer = L.layerGroup().addTo(map);
    map.on('moveend', () => {
      if (!map) return;
      const c = map.getCenter();
      saved = { lat: c.lat, lng: c.lng, zoom: map.getZoom() };
      refresh();
    });
    refresh();
  }

  // Re-query when the search or the filters change.
  $effect(() => {
    void store.rev;
    void store.query;
    refresh();
  });
</script>

<div class="mapwrap">
  <div class="map" bind:this={el}></div>
  {#if none && !anyLocated}
    <div class="hint">
      <Icon name="pin" size={18} />
      <div>
        <p><b>No photos with a location yet.</b></p>
        <p class="dim">Select photos in the grid and press <kbd>{locationKey}</kbd> to place them on a map, or use <i>Geotag from a track</i> under Library tools.</p>
      </div>
    </div>
  {:else if none}
    <div class="hint">
      <Icon name="filter" size={18} />
      <div>
        <p><b>No photo with a location matches.</b></p>
        <p class="dim">Clear the search or the filters to see them all.</p>
      </div>
    </div>
  {/if}
</div>

<style>
  .mapwrap {
    flex: 1;
    position: relative;
    min-height: 0;
    /* Leaflet's panes and controls stack up to z-index 1000: kept inside the map, so
       the menus, the single view, dialogs and toasts stay above it. */
    isolation: isolate;
  }
  .map {
    position: absolute;
    inset: 0;
  }
  :global([data-theme='dark']) .map :global(.leaflet-tile-pane) {
    filter: brightness(0.85) contrast(1.05);
  }
  .hint {
    position: absolute;
    left: 50%;
    bottom: 2rem;
    transform: translateX(-50%);
    z-index: 1001;
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    background: var(--raised);
    border-radius: var(--r-lg);
    padding: 0.75rem 1.125rem 0.75rem 0.875rem;
    box-shadow: var(--shadow-2);
    max-width: min(32rem, 90%);
    color: var(--text-2);
  }
  .hint :global(svg) {
    flex: none;
    margin-top: 0.125rem;
    color: var(--accent);
  }
  .hint p {
    margin: 0.125rem 0;
  }
  .hint b {
    color: var(--text);
  }
  /* Leaflet's controls in the app's colours. */
  .mapwrap :global(.leaflet-bar) {
    border: 0;
    border-radius: var(--r);
    box-shadow: var(--shadow-2);
    overflow: hidden;
  }
  .mapwrap :global(.leaflet-bar a) {
    display: grid;
    place-items: center;
    width: 2rem;
    height: 2rem;
    line-height: 2rem;
    background: var(--raised);
    color: var(--text);
    border-bottom: 1px solid var(--border);
    font-weight: 500;
    transition:
      background-color var(--dur-fast),
      color var(--dur-fast);
  }
  .mapwrap :global(.leaflet-bar a:last-child) {
    border-bottom: 0;
  }
  .mapwrap :global(.leaflet-bar a:hover) {
    background: var(--surface-3);
    color: var(--accent);
  }
  .mapwrap :global(.leaflet-bar a.leaflet-disabled) {
    background: var(--raised);
    color: var(--text-3);
    opacity: 0.6;
  }
  .mapwrap :global(.leaflet-control-attribution) {
    background: color-mix(in srgb, var(--raised) 82%, transparent);
    color: var(--text-3);
    border-top-left-radius: var(--r-sm);
  }
  .mapwrap :global(.leaflet-control-attribution a) {
    color: var(--text-2);
  }
  :global(.hf-marker) {
    background: none;
    border: 0;
  }
  :global(.hf-marker .mk) {
    width: 100%;
    height: 100%;
    border-radius: 0.5rem;
    overflow: visible;
    position: relative;
    box-shadow:
      0 0 0 2px #fff,
      0 3px 10px rgba(0, 0, 0, 0.45);
    background: #2a3540;
    transition: transform 0.12s;
  }
  :global(.hf-marker .mk:hover) {
    transform: scale(1.08);
  }
  :global(.hf-marker .mk img) {
    width: 100%;
    height: 100%;
    object-fit: cover;
    border-radius: 0.5rem;
  }
  :global(.hf-marker .badge) {
    position: absolute;
    top: -0.5rem;
    right: -0.5rem;
    box-shadow: 0 0 0 2px #fff;
  }
</style>
