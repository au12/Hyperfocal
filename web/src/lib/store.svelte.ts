import { SvelteSet } from 'svelte/reactivity';
import {
  api,
  emptyList,
  urls,
  type BatchResult,
  type Collection,
  type CollectionSpec,
  type DateRange,
  type Folder,
  type FolderRow,
  type IntegrityCounts,
  type JobStatus,
  type LabelColor,
  type Patch,
  type PhotoDetail,
  type PhotoList,
  type Status,
  type Timeline,
  type TrackInfo,
  type ToneMode,
  type WbMode,
} from './api';
import { keysFor, prettyKey, type Action, type KeyOverrides, type Preset } from './keys';
import { setOffline } from './offline';
import { people } from './people.svelte';

export const LABELS: { color: LabelColor; name: string; key: string }[] = [
  { color: 'red', name: 'Red', key: '6' },
  { color: 'yellow', name: 'Yellow', key: '7' },
  { color: 'green', name: 'Green', key: '8' },
  { color: 'blue', name: 'Blue', key: '9' },
  { color: 'purple', name: 'Purple', key: '' },
];

export type SortKey = 'taken' | 'name' | 'rating' | 'modified';

export interface Filters {
  rmin: number;
  unrated: boolean;
  lc: string[];
  pick: number | null;
  kinds: number[];
  gps: number | null;
  showRejected: boolean;
  onlyRejected: boolean;
  sort: SortKey;
  desc: boolean;
  cam: string;
  lens: string;
  kw: string;
  /** Capture date filter, 'YYYY-MM-DD' or '' (both ends inclusive). */
  dateFrom: string;
  dateTo: string;
  /** People and pets (subject ids): photos with all of them. */
  subj: number[];
}

export const DEFAULT_FILTERS: Filters = {
  rmin: 0,
  unrated: false,
  lc: [],
  pick: null,
  kinds: [],
  gps: null,
  showRejected: false,
  onlyRejected: false,
  sort: 'taken',
  desc: false,
  cam: '',
  lens: '',
  kw: '',
  dateFrom: '',
  dateTo: '',
  subj: [],
};

/** The filters that narrow what is shown (sort order and "show rejected" aside). */
export function activeFilterCount(f: Filters): number {
  return (
    (f.rmin > 0 ? 1 : 0) + (f.unrated ? 1 : 0) + (f.lc.length ? 1 : 0) + (f.pick != null ? 1 : 0) + (f.kinds.length ? 1 : 0) +
    (f.gps != null ? 1 : 0) + (f.cam ? 1 : 0) + (f.lens ? 1 : 0) + (f.kw ? 1 : 0) + (f.onlyRejected ? 1 : 0) + (f.dateFrom || f.dateTo ? 1 : 0) +
    (f.subj?.length ? 1 : 0)
  );
}

/** Built-in collections (the Library section). */
export const QUICK: { id: string; name: string; icon: string; spec: CollectionSpec }[] = [
  { id: 'picks', name: 'Picks', icon: 'flag', spec: { folder: '', rec: true, filters: { pick: 1 } } },
  { id: 'fivestar', name: 'Five stars', icon: 'star', spec: { folder: '', rec: true, filters: { rmin: 5 } } },
  { id: 'nogps', name: 'Without location', icon: 'pin', spec: { folder: '', rec: true, filters: { gps: 0 } } },
  { id: 'rejected', name: 'All rejected', icon: 'folder-x', spec: { folder: '', rec: true, filters: { onlyRejected: true, showRejected: true } } },
];

export interface ActiveCollection {
  /** A saved collection's id, or a built-in one's name. */
  id: number | string;
  name: string;
  spec: CollectionSpec;
}

const DAY = 86400;

/** Seconds (camera clock) at midnight of a 'YYYY-MM-DD' date. */
function dateSecs(s: string): number | null {
  const m = s.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  return m ? Date.UTC(+m[1], +m[2] - 1, +m[3]) / 1000 : null;
}

/** A readable name for a capture-date range: a year, a month, a day or from-to. */
export function rangeLabel(from: number, to: number): string {
  const a = new Date(from * 1000);
  const b = new Date(to * 1000);
  const opts = { timeZone: 'UTC' } as const;
  const y = a.getUTCFullYear(), m = a.getUTCMonth(), d = a.getUTCDate();
  if (Date.UTC(y + 1, 0, 1) === b.getTime() && m === 0 && d === 1) return String(y);
  if (Date.UTC(y, m + 1, 1) === b.getTime() && d === 1) return a.toLocaleDateString(undefined, { ...opts, month: 'long', year: 'numeric' });
  if (to - from === DAY) return a.toLocaleDateString(undefined, { ...opts, weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' });
  const last = new Date((to - DAY) * 1000);
  const f = (x: Date) => x.toLocaleDateString(undefined, { ...opts, day: 'numeric', month: 'short', year: 'numeric' });
  return `${f(a)} – ${f(last)}`;
}

/** The list query for a view (pure, so collection counts use the same rules). */
export function viewParams(v: {
  root: number | undefined;
  folderId: number | null;
  recursive: boolean;
  query: string;
  filters: Filters;
  dateRange: DateRange | null;
  /** A collection's scope applies to its search; a plain search covers the whole library. */
  scoped: boolean;
}): Record<string, unknown> {
  const f = v.filters;
  const q = v.query.trim();
  const wide = q.length > 0 && !v.scoped;
  let from: number | undefined;
  let to: number | undefined;
  const d = v.dateRange;
  if (d && !d.nodate) {
    from = d.from;
    to = d.to;
  }
  const df = f.dateFrom ? dateSecs(f.dateFrom) : null;
  const dt = f.dateTo ? dateSecs(f.dateTo) : null;
  if (df != null) from = from == null ? df : Math.max(from, df);
  if (dt != null) to = to == null ? dt + DAY : Math.min(to, dt + DAY);
  return {
    folder: wide ? v.root : (v.folderId ?? v.root),
    rec: wide ? 1 : v.recursive,
    rej: f.onlyRejected ? 2 : f.showRejected ? 1 : undefined,
    rmin: f.rmin > 0 ? f.rmin : undefined,
    rmax: f.unrated ? 0 : undefined,
    lc: f.lc.length ? f.lc.join(',') : undefined,
    pick: f.pick ?? undefined,
    kind: f.kinds.length ? f.kinds.join(',') : undefined,
    gps: f.gps ?? undefined,
    cam: f.cam || undefined,
    lens: f.lens || undefined,
    kw: f.kw || undefined,
    subj: f.subj?.length ? f.subj.join(',') : undefined,
    nodate: d?.nodate ? 1 : undefined,
    from,
    to,
    q: q || undefined,
    sort: f.sort === 'taken' ? undefined : f.sort,
    desc: f.desc,
  };
}

export interface ViewerState {
  source: 'jpeg' | 'raw';
  tone: ToneMode;
  wb: WbMode;
  ev: number;
  evOn: boolean;
  sharpen: 0 | 1 | 2;
  peaking: 0 | 1 | 2 | 3;
  over: boolean;
  under: boolean;
  invert: boolean;
}

export interface Settings {
  theme: 'dark' | 'light' | 'auto';
  defaultSource: 'jpeg' | 'raw';
  prefetch: number;
  keymap: 'frv' | 'lr';
  rowHeight: number;
  showNames: boolean;
  advanceAfterRate: boolean;
  advanceAfterReject: boolean;
  /** Screen sharpening amounts (unsharp mask, radius 1 / 2 device px). */
  sharpen1: number;
  sharpen2: number;
  peakColor: string;
  overColor: 'channels' | 'red';
  underDisplay: 'channels' | 'solid';
  underColor: string;
  bgGray: boolean;
  /** Full-resolution RAW view when zoomed in: demosaiced on this device's GPU, on the
   * server, either (auto: the GPU on desktops, the server on phones and tablets), or off. */
  rawDetail: 'auto' | 'gpu' | 'server' | 'off';
  /** The user's own shortcuts, per layout: the full key list of each action changed. */
  keyOverrides: Partial<Record<Preset, KeyOverrides>>;
  /** Keep thumbnails, previews and folder lists for offline use (needs HTTPS). */
  offlineCache: boolean;
}

/** Screen sharpening radius of levels 1 and 2, in device pixels. Radii and the default
 * amounts are fitted to FastRawViewer's screen sharpening (unsharp mask on display values). */
export const SHARPEN_SIGMA = [0, 1.0, 2.0] as const;

const DEFAULT_SETTINGS: Settings = {
  theme: 'dark',
  defaultSource: 'jpeg',
  prefetch: 3,
  keymap: 'frv',
  rowHeight: 200,
  showNames: false,
  advanceAfterRate: false,
  advanceAfterReject: true,
  sharpen1: 1.05,
  sharpen2: 1.03,
  peakColor: '#ff3b30',
  overColor: 'channels',
  underDisplay: 'channels',
  underColor: '#2f6fff',
  bgGray: false,
  rawDetail: 'auto',
  keyOverrides: {},
  offlineCache: true,
};

/** Saved settings still holding the previous, uncalibrated sharpening defaults get the new ones. */
function migrateSettings(s: Settings): Settings {
  if (s.sharpen1 === 0.55 && s.sharpen2 === 1.2) {
    s.sharpen1 = DEFAULT_SETTINGS.sharpen1;
    s.sharpen2 = DEFAULT_SETTINGS.sharpen2;
  }
  return s;
}

export interface Toast {
  id: number;
  kind: 'info' | 'ok' | 'error';
  text: string;
  action?: { label: string; run: () => void };
}

function loadLocal<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    if (v) return { ...fallback, ...JSON.parse(v) };
  } catch {
    /* ignore */
  }
  return fallback;
}

function saveLocal(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    /* ignore */
  }
}

export function applyTheme(t: Settings['theme']) {
  const light = t === 'light' || (t === 'auto' && matchMedia('(prefers-color-scheme: light)').matches);
  document.documentElement.dataset.theme = light ? 'light' : 'dark';
  document.querySelector('meta[name=theme-color]')?.setAttribute('content', light ? '#ffffff' : '#12171c');
  try {
    localStorage.setItem('hf.theme', t);
  } catch {
    /* ignore */
  }
}

let toastSeq = 1;

class Store {
  status = $state<Status | null>(null);
  needLogin = $state(false);

  // Folder tree
  roots = $state.raw<Folder[]>([]);
  folders = new Map<number, Folder>();
  treeRev = $state(0);
  expanded = new SvelteSet<number>();

  // Current scope
  folderId = $state<number | null>(null);
  recursive = $state(false);
  query = $state('');
  view = $state<'grid' | 'map' | 'people'>('grid');
  filters = $state<Filters>(loadLocal('hf.filters', { ...DEFAULT_FILTERS }));
  /** A timeline period being browsed (whole library), or null. */
  dateRange = $state<DateRange | null>(null);
  /** The collection being shown (its filters replace the user's until it is left). */
  collection = $state.raw<ActiveCollection | null>(null);
  private savedFilters: Filters | null = null;
  collections = $state.raw<Collection[]>([]);
  /** Photos per collection (saved ids and built-in names). */
  counts = $state<Record<string, number>>({});
  timeline = $state.raw<Timeline | null>(null);
  /** Folder path -> id. */
  pathIds = new Map<string, number>();

  // Photo list (columnar, not deeply reactive; `rev` signals in-place changes)
  list = $state.raw<PhotoList>(emptyList());
  rev = $state(0);
  loading = $state(false);
  listError = $state<string | null>(null);
  private listAbort: AbortController | null = null;
  index = new Map<number, number>();

  /** Explicitly selected photos. Empty means the photo at the cursor is the selection,
   * as in a file manager where the focused item is the selected one. */
  selected = new SvelteSet<number>();
  cursor = $state(0);
  /** Shift ranges: the photo a range starts from, the photo it last ended on, and what
   * stays selected around the range (the selection when the anchor was set). Ids, so they
   * survive reloads. A range re-anchors at the focus whenever the focus moved some other
   * way since the last selection change. */
  private anchorId: number | null = null;
  private headId: number | null = null;
  private rangeBase = new Set<number>();

  // Single view
  loupe = $state(false);
  current = $state(0);
  /** Compare view: 2-4 photos side by side (ids, in pane order) and the active pane. */
  compare = $state<number[] | null>(null);
  compareActive = $state(0);
  detail = $state<PhotoDetail | null>(null);
  private detailAbort: AbortController | null = null;
  viewer = $state<ViewerState>(
    loadLocal('hf.viewer', { source: 'jpeg', tone: 'normal', wb: 'asshot', ev: 0, evOn: true, sharpen: 0, peaking: 0, over: false, under: false, invert: false } as ViewerState),
  );
  panels = $state(loadLocal('hf.panels', { sidebar: true, info: true, histogram: false, filmstrip: true, hidden: false, faces: false }));

  settings = $state<Settings>(migrateSettings(loadLocal('hf.settings', DEFAULT_SETTINGS)));
  toasts = $state<Toast[]>([]);
  dialog = $state<null | 'settings' | 'help' | 'journal' | 'location' | 'keywords' | 'integrity' | 'geotag'>(null);
  /** Background changes, by id (running and recently finished). */
  jobs = $state<Record<string, JobStatus>>({});
  private jobsSeen = new Set<string>();
  private jobsDone = new Set<string>();
  locationTargets = $state.raw<number[]>([]);
  /** Track files loaded for geotagging (this page session). */
  tracks = $state<TrackInfo[]>([]);
  /** Thumbnails replaced since they were first shown (a HEIC draft), by photo id. */
  thumbRev = $state<Record<number, number>>({});
  /** Archive integrity problems (kept current by the event stream). */
  integrity = $state<IntegrityCounts>({ changed: 0, missing: 0, unreadable: 0 });
  mobile = $state(false);
  busy = $state(0);
  /** Showing offline copies: the server could not be reached. */
  offline = $state(false);
  /** Names of the people and pets in the photo filter, by id. */
  subjLabels = $state<Record<number, string>>(loadLocal('hf.subj', {} as Record<number, string>));

  private es: EventSource | null = null;
  private reloadTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    if (typeof window !== 'undefined') {
      const mq = matchMedia('(max-width: 760px)');
      this.mobile = mq.matches;
      mq.addEventListener('change', (e) => (this.mobile = e.matches));
      if (!localStorage.getItem('hf.viewer')) this.viewer.source = this.settings.defaultSource;
      window.addEventListener('hf-auth', () => (this.needLogin = true));
      window.addEventListener('hf-net', (e) => (this.offline = (e as CustomEvent<boolean>).detail));
    }
  }

  /** The user's shortcut changes for the current layout. */
  keyOverrides = (): KeyOverrides => this.settings.keyOverrides?.[this.settings.keymap] ?? {};

  setKeyOverrides = (ov: KeyOverrides) => {
    this.settings.keyOverrides = { ...(this.settings.keyOverrides ?? {}), [this.settings.keymap]: ov };
    this.persist();
  };

  /** " (key)" for a tooltip: the first key of an action in the current bindings. */
  keyHint = (a: Action): string => {
    const k = keysFor(a, this.settings.keymap, this.keyOverrides())[0];
    return k ? ` (${prettyKey(k)})` : '';
  };

  persist = () => {
    saveLocal('hf.settings', this.settings);
    saveLocal('hf.viewer', this.viewer);
    saveLocal('hf.panels', this.panels);
    // A collection's filters are its own; the user's come back when it is left.
    saveLocal('hf.filters', this.savedFilters ?? this.filters);
  };

  // ------------------------------------------------------------ toasts
  toast = (text: string, kind: Toast['kind'] = 'info', action?: Toast['action'], ms = 4500) => {
    const id = toastSeq++;
    this.toasts = [...this.toasts, { id, kind, text, action }];
    setTimeout(() => (this.toasts = this.toasts.filter((t) => t.id !== id)), ms);
  };

  // ------------------------------------------------------------ boot
  init = async () => {
    applyTheme(this.settings.theme);
    setOffline(this.settings.offlineCache);
    try {
      this.status = await api.status();
      if (this.status.auth && !this.status.logged_in) {
        this.needLogin = true;
        return;
      }
      if (this.status.counts?.integrity) this.integrity = this.status.counts.integrity;
    } catch (e) {
      this.toast(`Server unreachable: ${(e as Error).message}`, 'error');
      return;
    }
    await this.loadTree();
    this.connectEvents();
    people.loadStatus();
    await this.loadCollections().catch(() => {});
    this.loadTimeline();
    const hash = new URLSearchParams(location.hash.slice(1));
    const f = Number(hash.get('f'));
    const c = hash.get('c');
    const d = hash.get('d');
    const saved = c ? this.collections.find((x) => String(x.id) === c) : undefined;
    const quick = c ? QUICK.find((x) => x.id === c) : undefined;
    if (saved || quick) {
      await this.applyCollection(saved ? { id: saved.id, name: saved.name, spec: saved.spec } : { id: quick!.id, name: quick!.name, spec: quick!.spec });
    } else if (d) {
      const m = d.match(/^(-?\d+)_(-?\d+)$/);
      if (d === 'none') await this.openDate({ from: 0, to: 0, label: 'No date', nodate: true });
      else if (m) await this.openDate({ from: +m[1], to: +m[2], label: rangeLabel(+m[1], +m[2]) });
    } else {
      const start = this.folders.has(f) ? f : (this.lastFolder() ?? this.roots[0]?.id ?? null);
      if (start != null) await this.openFolder(start, false);
    }
    const p = Number(hash.get('p'));
    if (p && this.index.has(p)) this.openLoupe(this.index.get(p)!);
    window.addEventListener('hashchange', this.onHash);
  };

  private lastFolder(): number | null {
    const v = Number(localStorage.getItem('hf.folder'));
    return this.folders.has(v) ? v : null;
  }

  private onHash = () => {
    const hash = new URLSearchParams(location.hash.slice(1));
    const p = Number(hash.get('p'));
    if (!p && this.loupe) this.loupe = false;
  };

  private writeHash() {
    const h = new URLSearchParams();
    if (this.collection) h.set('c', String(this.collection.id));
    else if (this.dateRange) h.set('d', this.dateRange.nodate ? 'none' : `${this.dateRange.from}_${this.dateRange.to}`);
    else if (this.folderId != null) h.set('f', String(this.folderId));
    if (this.loupe && this.list.id[this.current] != null) h.set('p', String(this.list.id[this.current]));
    const s = '#' + h.toString();
    if (location.hash !== s) history.replaceState(null, '', s);
  }

  connectEvents = () => {
    this.es?.close();
    const es = new EventSource(urls.events());
    this.es = es;
    es.onmessage = (m) => {
      let e: any;
      try {
        e = JSON.parse(m.data);
      } catch {
        return;
      }
      if (e.type === 'progress' && this.status) {
        const { type, ...p } = e;
        this.status = { ...this.status, progress: p };
      } else if (e.type === 'tree') {
        this.loadTree();
        this.scheduleTimeline();
      } else if (e.type === 'collections') {
        this.loadCollections().catch(() => {});
      } else if (e.type === 'folder') {
        if (this.affects(e.id)) this.scheduleReload();
        this.scheduleTree();
      } else if (e.type === 'photos') {
        const ids: number[] = e.ids ?? [];
        if (ids.some((i) => this.index.has(i))) this.scheduleReload();
        if (this.detail && ids.includes(this.detail.id)) this.loadDetail();
      } else if (e.type === 'job') {
        const { type, ...j } = e;
        this.onJob(j as JobStatus);
      } else if (e.type === 'thumbs') {
        for (const id of (e.ids ?? []) as number[]) this.thumbRev[id] = (this.thumbRev[id] ?? 0) + 1;
      } else if (e.type === 'integrity') {
        this.integrity = { changed: e.changed ?? 0, missing: e.missing ?? 0, unreadable: e.unreadable ?? 0 };
      } else if (e.type === 'people' || e.type === 'people_status') {
        people.onEvent(e);
      }
    };
    es.onerror = () => {
      /* EventSource reconnects by itself */
    };
  };

  /** A background change moved on: progress in the header, a toast when it is done. */
  onJob = (j: JobStatus) => {
    // Updates can arrive late or twice (the event stream and the API response).
    if (this.jobsDone.has(j.id)) return;
    const prev = this.jobs[j.id];
    if (prev && !j.finished && prev.done > j.done) return;
    const seenRunning = this.jobsSeen.has(j.id);
    this.jobs[j.id] = j;
    if (!j.finished) {
      this.jobsSeen.add(j.id);
      return;
    }
    this.jobsDone.add(j.id);
    if (!seenRunning) {
      delete this.jobs[j.id];
      return;
    }
    this.jobsSeen.delete(j.id);
    const ok = j.done - j.failed;
    if (j.failed) this.toast(`${j.label}: ${j.failed} of ${j.total} not saved: ${j.errors[0]?.[1] ?? ''}`, 'error', { label: 'Undo the rest', run: () => this.undo(j.batch) }, 12000);
    else this.toast(`${j.label}: ${ok.toLocaleString()} photo${ok === 1 ? '' : 's'} updated`, 'ok', { label: 'Undo', run: () => this.undo(j.batch) }, 9000);
    this.scheduleReload();
    if (this.loupe || this.detail) this.loadDetail();
    setTimeout(() => {
      if (this.jobs[j.id]?.finished) delete this.jobs[j.id];
    }, 4000);
  };

  /** Background changes still running. */
  runningJobs = (): JobStatus[] => Object.values(this.jobs).filter((j) => !j.finished);

  private treeTimer: ReturnType<typeof setTimeout> | null = null;
  private scheduleTree() {
    if (this.treeTimer) clearTimeout(this.treeTimer);
    this.treeTimer = setTimeout(() => {
      this.loadTree();
      this.loadTimeline();
      this.refreshCounts();
    }, 1500);
  }

  private timelineTimer: ReturnType<typeof setTimeout> | null = null;
  private scheduleTimeline() {
    if (this.timelineTimer) clearTimeout(this.timelineTimer);
    this.timelineTimer = setTimeout(() => this.loadTimeline(), 1500);
  }

  loadTimeline = async () => {
    try {
      this.timeline = await api.timeline();
    } catch {
      /* keep the last one */
    }
  };

  private affects(folderId: number): boolean {
    if (this.query || this.dateRange || this.collection) return true;
    if (this.folderId == null) return false;
    if (folderId === this.folderId) return true;
    if (!this.recursive) return false;
    let f = this.folders.get(folderId);
    while (f) {
      if (f.id === this.folderId) return true;
      f = f.parent != null ? this.folders.get(f.parent) : undefined;
    }
    return false;
  }

  scheduleReload = () => {
    if (this.reloadTimer) clearTimeout(this.reloadTimer);
    this.reloadTimer = setTimeout(() => this.reload(true), 400);
  };

  // ------------------------------------------------------------ tree
  loadTree = async () => {
    const { folders } = await api.tree();
    const map = new Map<number, Folder>();
    const roots: Folder[] = [];
    for (const [id, parent, name, count, rej] of folders as FolderRow[]) {
      map.set(id, { id, parent, name, count, isReject: !!rej, children: [], total: count, path: '', depth: 0 });
    }
    for (const f of map.values()) {
      const p = f.parent != null ? map.get(f.parent) : undefined;
      if (p) p.children.push(f);
      else roots.push(f);
    }
    const walk = (f: Folder, path: string, depth: number): number => {
      f.path = path;
      f.depth = depth;
      f.children.sort((a, b) => {
        if (a.isReject !== b.isReject) return a.isReject ? 1 : -1;
        const na = Number(a.name), nb = Number(b.name);
        if (!isNaN(na) && !isNaN(nb)) return nb - na; // years: newest first
        return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
      });
      let t = f.count;
      for (const c of f.children) t += c.isReject ? 0 : walk(c, path ? `${path}/${c.name}` : c.name, depth + 1);
      for (const c of f.children) if (c.isReject) walk(c, path ? `${path}/${c.name}` : c.name, depth + 1);
      f.total = t;
      return t;
    };
    for (const r of roots) walk(r, '', 0);
    const paths = new Map<string, number>();
    for (const f of map.values()) paths.set(f.path, f.id);
    this.pathIds = paths;
    this.folders = map;
    this.roots = roots;
    for (const r of roots) this.expanded.add(r.id);
    this.treeRev++;
  };

  folderPath = (id: number | null): Folder[] => {
    const out: Folder[] = [];
    let f = id != null ? this.folders.get(id) : undefined;
    while (f) {
      out.unshift(f);
      f = f.parent != null ? this.folders.get(f.parent) : undefined;
    }
    return out;
  };

  // ------------------------------------------------------------ list
  openFolder = async (id: number, fromUser = true) => {
    if (fromUser) {
      this.query = '';
      this.dateRange = null;
      this.leaveCollection();
    }
    this.folderId = id;
    try {
      localStorage.setItem('hf.folder', String(id));
    } catch {
      /* ignore */
    }
    for (const f of this.folderPath(id)) this.expanded.add(f.id);
    const f = this.folders.get(id);
    // A folder without photos of its own (the library root, a year): show everything below it.
    if (f && f.count === 0 && f.children.length > 0) this.recursive = true;
    else if (f && f.count > 0 && fromUser) this.recursive = false;
    this.loupe = false;
    this.compare = null;
    this.clearSelection();
    await this.reload();
    this.cursor = 0;
    this.writeHash();
  };

  /** Every folder of the library at once (no search, period or collection).
   * `prepare` runs before the list loads, e.g. to set a filter. */
  openLibrary = async (prepare?: () => void) => {
    const root = this.roots[0]?.id;
    if (root == null) return;
    this.leaveCollection();
    this.query = '';
    this.dateRange = null;
    prepare?.();
    this.folderId = root;
    this.recursive = true;
    this.loupe = false;
    this.compare = null;
    this.clearSelection();
    try {
      localStorage.setItem('hf.folder', String(root));
    } catch {
      /* ignore */
    }
    await this.reload();
    this.cursor = 0;
    this.writeHash();
  };

  /** The photos of a person or pet (a group), across the library. */
  showSubject = (id: number, label: string) => {
    this.subjLabels[id] = label;
    saveLocal('hf.subj', this.subjLabels);
    this.view = 'grid';
    return this.openLibrary(() => {
      this.filters.subj = [id];
      this.persist();
    });
  };

  /** Switch to (or leave) the People & pets view. */
  openPeople = () => {
    this.loupe = false;
    this.compare = null;
    this.view = 'people';
  };

  /** The photos with a keyword (or one below it), across the library. */
  showKeyword = (kw: string) => {
    if (this.view === 'map') this.view = 'grid';
    return this.openLibrary(() => {
      this.filters.kw = kw;
      this.persist();
    });
  };

  params = (): Record<string, unknown> =>
    viewParams({
      root: this.roots[0]?.id,
      folderId: this.folderId,
      recursive: this.recursive,
      query: this.query,
      filters: this.filters,
      dateRange: this.dateRange,
      scoped: this.collection != null,
    });

  // ------------------------------------------------------------ timeline & collections
  /** Browse one period of the timeline across every folder. */
  openDate = async (range: DateRange) => {
    this.leaveCollection();
    this.query = '';
    this.dateRange = range;
    this.folderId = this.roots[0]?.id ?? null;
    this.recursive = true;
    this.loupe = false;
    this.compare = null;
    this.clearSelection();
    await this.reload();
    this.cursor = 0;
    this.writeHash();
  };

  private leaveCollection() {
    if (!this.collection) return;
    if (this.savedFilters) this.filters = this.savedFilters;
    this.savedFilters = null;
    this.collection = null;
    this.persist();
  }

  /** Show a collection: its search, scope, dates and filters (the user's own filters
   * come back when it is left). */
  applyCollection = async (c: ActiveCollection) => {
    if (!this.collection) this.savedFilters = $state.snapshot(this.filters) as Filters;
    const sp = c.spec;
    const base = this.savedFilters ?? DEFAULT_FILTERS;
    this.collection = c;
    this.filters = { ...DEFAULT_FILTERS, sort: base.sort, desc: base.desc, ...(sp.filters as Partial<Filters>) };
    this.query = sp.q ?? '';
    this.dateRange = sp.date ?? null;
    const root = this.roots[0]?.id ?? null;
    const fid = sp.folder ? this.pathIds.get(sp.folder) : root;
    if (sp.folder && fid == null) this.toast(`The folder “${sp.folder}” is no longer in the library: showing the whole library`, 'info');
    this.folderId = fid ?? root;
    this.recursive = sp.rec ?? true;
    this.loupe = false;
    this.compare = null;
    this.clearSelection();
    await this.reload();
    this.cursor = 0;
    this.writeHash();
  };

  /** What the current view would save as. */
  currentSpec = (): CollectionSpec => {
    const wide = this.query.trim() !== '' && !this.collection;
    return {
      q: this.query.trim() || undefined,
      folder: wide ? '' : (this.folders.get(this.folderId ?? -1)?.path ?? ''),
      rec: wide ? true : this.recursive,
      filters: $state.snapshot(this.filters) as unknown as Record<string, unknown>,
      date: this.dateRange ? ($state.snapshot(this.dateRange) as DateRange) : null,
    };
  };

  /** Whether the saved collection on screen no longer matches its spec. */
  collectionModified = (): boolean => {
    const c = this.collection;
    if (!c || typeof c.id !== 'number') return false;
    const norm = (sp: CollectionSpec) =>
      JSON.stringify({
        q: sp.q ?? '',
        folder: sp.folder ?? '',
        rec: sp.rec ?? true,
        filters: { ...DEFAULT_FILTERS, ...(sp.filters as Partial<Filters>) },
        date: sp.date && !sp.date.nodate ? [sp.date.from, sp.date.to] : sp.date?.nodate ? 'none' : null,
      });
    void this.rev;
    return norm(c.spec) !== norm(this.currentSpec());
  };

  loadCollections = async () => {
    this.collections = await api.collections();
    if (this.collection && typeof this.collection.id === 'number') {
      const now = this.collections.find((x) => x.id === this.collection!.id);
      if (now) this.collection = { id: now.id, name: now.name, spec: now.spec };
    }
    this.refreshCounts(true);
  };

  /** Save the current view as a new collection, or over `id`. */
  saveCollection = async (name: string, id?: number) => {
    try {
      const c = await api.saveCollection({ id, name, spec: this.currentSpec() });
      if (!this.collection) this.savedFilters = $state.snapshot(this.filters) as Filters;
      this.collection = { id: c.id, name: c.name, spec: c.spec };
      this.writeHash();
      await this.loadCollections();
      this.toast(id ? `Updated “${c.name}”` : `Saved “${c.name}”`, 'ok');
    } catch (e) {
      this.toast(`Not saved: ${(e as Error).message}`, 'error');
    }
  };

  renameCollection = async (c: Collection, name: string) => {
    try {
      await api.saveCollection({ id: c.id, name, spec: c.spec });
      await this.loadCollections();
    } catch (e) {
      this.toast(`Not renamed: ${(e as Error).message}`, 'error');
    }
  };

  deleteCollection = async (c: Collection) => {
    try {
      await api.deleteCollection(c.id);
      if (this.collection?.id === c.id) {
        this.leaveCollection();
        await this.reload();
        this.writeHash();
      }
      await this.loadCollections();
      this.toast(`Deleted “${c.name}”`, 'ok');
    } catch (e) {
      this.toast(`Not deleted: ${(e as Error).message}`, 'error');
    }
  };

  /** The list query of a collection, as `params()` would build it while it is shown. */
  specParams = (sp: CollectionSpec): Record<string, unknown> => {
    const root = this.roots[0]?.id;
    const fid = sp.folder ? (this.pathIds.get(sp.folder) ?? root) : root;
    const base = this.savedFilters ?? this.filters;
    return viewParams({
      root,
      folderId: fid ?? null,
      recursive: sp.rec ?? true,
      query: sp.q ?? '',
      filters: { ...DEFAULT_FILTERS, sort: base.sort, desc: base.desc, ...(sp.filters as Partial<Filters>) },
      dateRange: sp.date ?? null,
      scoped: true,
    });
  };

  private countsAt = 0;
  private countsAbort: AbortController | null = null;
  /** Photo counts of every collection (at most every 20 s unless forced). */
  refreshCounts = async (force = false) => {
    if (!this.roots.length || (!force && Date.now() - this.countsAt < 20_000)) return;
    this.countsAt = Date.now();
    this.countsAbort?.abort();
    const ctl = new AbortController();
    this.countsAbort = ctl;
    const all: [string, CollectionSpec][] = [...QUICK.map((q) => [q.id, q.spec] as [string, CollectionSpec]), ...this.collections.map((c) => [String(c.id), c.spec] as [string, CollectionSpec])];
    await Promise.all(
      all.map(async ([id, sp]) => {
        try {
          const { count } = await api.count(this.specParams(sp), ctl.signal);
          if (!ctl.signal.aborted) this.counts[id] = count;
        } catch {
          /* aborted or failed */
        }
      }),
    );
  };

  reload = async (keepPosition = false) => {
    this.listAbort?.abort();
    const ctl = new AbortController();
    this.listAbort = ctl;
    const keepId = keepPosition ? this.list.id[this.current] : undefined;
    const keepCursor = keepPosition ? this.list.id[this.cursor] : undefined;
    this.loading = true;
    this.listError = null;
    try {
      const list = await api.photos(this.params(), ctl.signal);
      if (ctl.signal.aborted) return;
      this.list = list;
      const idx = new Map<number, number>();
      list.id.forEach((id, i) => idx.set(id, i));
      this.index = idx;
      for (const s of [...this.selected]) if (!idx.has(s)) this.selected.delete(s);
      if (keepId != null && idx.has(keepId)) this.current = idx.get(keepId)!;
      else if (this.current >= list.id.length) this.current = Math.max(0, list.id.length - 1);
      if (keepCursor != null && idx.has(keepCursor)) this.cursor = idx.get(keepCursor)!;
      else if (this.cursor >= list.id.length) this.cursor = Math.max(0, list.id.length - 1);
      if (this.loupe && list.id.length === 0) this.loupe = false;
      this.pruneCompare();
      this.rev++;
    } catch (e) {
      if ((e as Error).name !== 'AbortError') this.listError = (e as Error).message;
    } finally {
      if (this.listAbort === ctl) this.loading = false;
    }
  };

  setFilter = <K extends keyof Filters>(k: K, v: Filters[K]) => {
    this.filters[k] = v;
    this.persist();
    this.reload();
  };

  search = (q: string) => {
    this.query = q;
    this.loupe = false;
    this.compare = null;
    this.reload();
    this.writeHash();
  };

  // ------------------------------------------------------------ selection
  targets = (): number[] => {
    if (this.compare) {
      const id = this.compare[this.compareActive];
      return id != null ? [id] : [];
    }
    if (this.loupe) {
      const id = this.list.id[this.current];
      return id != null ? [id] : [];
    }
    if (this.selected.size > 0) return [...this.selected];
    const id = this.list.id[this.cursor];
    return id != null ? [id] : [];
  };

  /** The photo the keyboard and the info panel act on: the single view's photo, or the
   * grid cursor. */
  private focusIndex = () => (this.loupe ? this.current : this.cursor);

  private setAnchor(i: number) {
    const id = this.list.id[i] ?? null;
    this.anchorId = this.headId = id;
    this.rangeBase = new Set(this.selected);
    if (id != null) this.rangeBase.delete(id);
  }

  /** Replaces the selection, touching only the photos that change. */
  private setSelection(next: Set<number>) {
    for (const id of [...this.selected]) if (!next.has(id)) this.selected.delete(id);
    for (const id of next) if (!this.selected.has(id)) this.selected.add(id);
  }

  /** A selection of just the photo at the cursor is the same as no selection (the cursor
   * photo is the target either way); keeping it empty avoids a lone check mark. */
  private normalize() {
    if (this.selected.size === 1 && this.selected.has(this.list.id[this.cursor])) this.selected.clear();
  }

  /** Plain click: that photo alone. */
  clickSelect = (i: number) => {
    if (this.list.id[i] == null) return;
    this.selected.clear();
    this.cursor = i;
    this.setAnchor(i);
  };

  /** Adds or removes one photo and puts the cursor (and the range anchor) on it.
   * `withCursor` (Ctrl/Cmd-click): when nothing is selected yet, the photo at the cursor
   * counts as selected, so click A then Ctrl-click B gives A and B. */
  select = (i: number, mode: 'toggle', withCursor = false) => {
    void mode;
    const id = this.list.id[i];
    if (id == null) return;
    if (withCursor && this.selected.size === 0 && !this.loupe) {
      const c = this.list.id[this.cursor];
      if (c != null && c !== id) this.selected.add(c);
    }
    if (this.selected.has(id)) this.selected.delete(id);
    else this.selected.add(id);
    this.cursor = i;
    this.setAnchor(i);
  };

  /** Shift-click and Shift+arrows: selects from the anchor to `to` (both included) on
   * top of what was selected besides the range, and moves the focus there. Going back
   * towards the anchor shrinks the range again. */
  extendTo = (to: number) => {
    const L = this.list;
    if (to < 0 || to >= L.id.length) return;
    const f = this.focusIndex();
    if (this.anchorId == null || this.headId !== L.id[f] || !this.index.has(this.anchorId)) this.setAnchor(f);
    const a = this.index.get(this.anchorId!)!;
    const [lo, hi] = a < to ? [a, to] : [to, a];
    const next = new Set(this.rangeBase);
    for (let k = lo; k <= hi; k++) next.add(L.id[k]);
    this.setSelection(next);
    this.headId = L.id[to];
    if (this.loupe && to !== this.current) {
      this.current = to;
      this.loadDetail();
      this.writeHash();
    }
    this.cursor = to;
    this.normalize();
  };

  /** The selection a Ctrl/Shift rubber band starts from (the cursor photo when nothing
   * is selected). */
  marqueeBase = (): Set<number> => {
    if (this.selected.size) return new Set(this.selected);
    const c = this.list.id[this.cursor];
    return new Set(c != null ? [c] : []);
  };

  /** Rubber-band selection: `hits` are the indices inside the band. 'replace' selects just
   * those, 'add' adds them to `base`, 'toggle' flips them against `base`. */
  marquee = (hits: number[], mode: 'replace' | 'add' | 'toggle', base: Set<number>) => {
    const next = mode === 'replace' ? new Set<number>() : new Set(base);
    for (const i of hits) {
      const id = this.list.id[i];
      if (mode === 'toggle' && base.has(id)) next.delete(id);
      else next.add(id);
    }
    this.setSelection(next);
  };

  /** Ends a rubber band: the cursor goes to `i` (a photo near where the band ended). */
  marqueeEnd = (i: number | null) => {
    if (i != null && this.list.id[i] != null) this.cursor = i;
    this.normalize();
    this.setAnchor(this.cursor);
  };

  /** Moves the grid cursor without Shift/Ctrl: the selection collapses to that photo. */
  moveCursor = (i: number) => {
    if (i < 0 || i >= this.list.id.length) return;
    if (this.selected.size) this.clearSelection();
    this.cursor = i;
  };

  selectAll = () => {
    for (const id of this.list.id) this.selected.add(id);
    this.anchorId = this.headId = this.list.id[this.focusIndex()] ?? null;
    this.rangeBase = new Set();
  };
  clearSelection = () => {
    this.selected.clear();
    this.rangeBase = new Set();
    this.headId = null;
  };
  invertSelection = () => {
    for (const id of this.list.id) {
      if (this.selected.has(id)) this.selected.delete(id);
      else this.selected.add(id);
    }
    this.headId = null;
  };

  // ------------------------------------------------------------ single view
  openLoupe = (i: number) => {
    if (i < 0 || i >= this.list.id.length) return;
    this.current = i;
    this.cursor = i;
    this.loupe = true;
    this.loadDetail();
    this.writeHash();
  };

  closeLoupe = () => {
    this.loupe = false;
    this.cursor = this.current;
    this.writeHash();
  };

  // ------------------------------------------------------------ compare view
  /** Compares the selected photos (2-4; the first 4 of more), or else the photo at the
   * cursor (or in the single view) and the next one. */
  openCompare = () => {
    const L = this.list;
    let ids = [...this.selected].filter((id) => this.index.has(id)).sort((a, b) => this.index.get(a)! - this.index.get(b)!);
    if (ids.length > 4) {
      this.toast(`Comparing the first 4 of ${ids.length} selected photos`, 'info');
      ids = ids.slice(0, 4);
    }
    if (ids.length < 2) {
      const from = this.loupe ? this.current : this.cursor;
      ids = [L.id[from], L.id[from + 1] ?? L.id[from - 1]].filter((x) => x != null);
    }
    if (ids.length < 2) {
      this.toast('Select 2 to 4 photos to compare', 'info');
      return;
    }
    this.compare = ids;
    this.compareActive = 0;
    this.loupe = false;
    this.cursor = this.index.get(ids[0]) ?? this.cursor;
    this.loadDetail();
  };

  closeCompare = () => {
    const id = this.compare?.[this.compareActive];
    this.compare = null;
    const i = id != null ? this.index.get(id) : undefined;
    if (i != null) this.cursor = i;
    this.loadDetail();
  };

  setCompareActive = (k: number) => {
    if (!this.compare || k < 0 || k >= this.compare.length) return;
    this.compareActive = k;
    const i = this.index.get(this.compare[k]);
    if (i != null) this.cursor = i;
    this.loadDetail();
  };

  /** The active pane shows the next (d = 1) or previous photo of the list that is not
   * already compared: stepping a candidate through a burst against a keeper. */
  compareStep = (d: 1 | -1) => {
    const c = this.compare;
    if (!c) return;
    const from = this.index.get(c[this.compareActive]);
    if (from == null) return;
    for (let i = from + d; i >= 0 && i < this.list.id.length; i += d) {
      const id = this.list.id[i];
      if (!c.includes(id)) {
        const next = [...c];
        next[this.compareActive] = id;
        this.compare = next;
        this.cursor = i;
        this.loadDetail();
        return;
      }
    }
  };

  compareRemove = (k: number) => {
    const c = this.compare;
    if (!c) return;
    const next = c.filter((_, j) => j !== k);
    if (next.length < 2) return this.closeCompare();
    this.compare = next;
    this.setCompareActive(Math.min(this.compareActive, next.length - 1));
  };

  /** Drops compared photos that left the list; closes the view below two. */
  private pruneCompare() {
    const c = this.compare;
    if (!c) return;
    const next = c.filter((id) => this.index.has(id));
    if (next.length === c.length) return;
    if (next.length < 2) {
      this.compare = null;
      return;
    }
    this.compare = next;
    this.compareActive = Math.min(this.compareActive, next.length - 1);
  }

  /** Next/previous photo. In the grid, as in any file manager, a plain move collapses the
   * selection to the photo moved to; `keep` (select-and-next) leaves it alone. */
  go = (delta: number, keep = false) => {
    const n = this.list.id.length;
    if (!n) return;
    const i = Math.max(0, Math.min(n - 1, (this.loupe ? this.current : this.cursor) + delta));
    if (this.loupe) {
      if (i !== this.current) {
        this.current = i;
        this.cursor = i;
        this.loadDetail();
        this.writeHash();
      }
    } else {
      if (!keep && !this.compare && this.selected.size) this.clearSelection();
      this.cursor = i;
    }
  };

  goTo = (i: number) => this.go(i - (this.loupe ? this.current : this.cursor));

  loadDetail = async () => {
    const id = this.focusId();
    if (id == null) {
      this.detail = null;
      return;
    }
    this.detailAbort?.abort();
    const ctl = new AbortController();
    this.detailAbort = ctl;
    try {
      const d = await api.photo(id, ctl.signal);
      if (!ctl.signal.aborted) this.detail = d;
    } catch {
      /* aborted or failed */
    }
  };

  /** Detail for the photo the info panel should show (grid cursor, loupe or the active
   * compare pane). */
  focusId = (): number | undefined => (this.compare ? this.compare[this.compareActive] : this.list.id[this.loupe ? this.current : this.cursor]);

  // ------------------------------------------------------------ edits
  private optimistic(ids: number[], patch: Patch): () => void {
    const L = this.list;
    const undo: [number, number, LabelColor | null, number][] = [];
    for (const id of ids) {
      const i = this.index.get(id);
      if (i == null) continue;
      undo.push([i, L.r[i], L.lc[i], L.p[i]]);
      if (patch.rating !== undefined) L.r[i] = patch.rating;
      if (patch.label !== undefined) L.lc[i] = (patch.label?.toLowerCase() as LabelColor) ?? null;
      if (patch.pick !== undefined) L.p[i] = patch.pick;
    }
    if (this.detail && ids.includes(this.detail.id)) {
      const d = { ...this.detail };
      if (patch.rating !== undefined) d.rating = patch.rating;
      if (patch.label !== undefined) {
        d.label = patch.label;
        d.lcolor = (patch.label?.toLowerCase() as LabelColor) ?? null;
      }
      if (patch.pick !== undefined) d.pick = patch.pick;
      this.detail = d;
    }
    this.rev++;
    return () => {
      for (const [i, r, lc, p] of undo) {
        L.r[i] = r;
        L.lc[i] = lc;
        L.p[i] = p;
      }
      this.rev++;
    };
  }

  private readonlyGuard(): boolean {
    if (this.status?.readonly) {
      this.toast('Read-only mode: changes are disabled', 'error');
      return true;
    }
    return false;
  }

  patch = async (ids: number[], patch: Patch, describe?: string): Promise<BatchResult | null> => {
    if (!ids.length || this.readonlyGuard()) return null;
    const revert = this.optimistic(ids, patch);
    this.busy++;
    try {
      const r = await api.meta(ids, patch);
      if (r.failed.length) {
        revert();
        this.toast(`${r.failed.length} of ${ids.length} not saved: ${r.failed[0][1]}`, 'error', undefined, 8000);
      } else if (describe) {
        this.toast(describe, 'ok', { label: 'Undo', run: () => this.undo(r.batch) });
      }
      if (this.detail && ids.includes(this.detail.id)) this.loadDetail();
      return r;
    } catch (e) {
      revert();
      this.toast(`Not saved: ${(e as Error).message}`, 'error', undefined, 8000);
      return null;
    } finally {
      this.busy--;
    }
  };

  rate = (r: number) => {
    const ids = this.targets();
    const cur = this.list.r[this.index.get(ids[0]) ?? -1];
    const value = ids.length === 1 && cur === r && r > 0 ? 0 : r;
    this.patch(ids, { rating: value }, ids.length > 1 ? `Rated ${ids.length} photos ${value}★` : undefined);
    if (this.settings.advanceAfterRate && ids.length === 1 && !this.compare) this.go(1);
  };

  bumpRating = (d: number) => {
    const ids = this.targets();
    const i = this.index.get(ids[0]);
    if (i == null) return;
    this.rate(Math.max(0, Math.min(5, this.list.r[i] + d)));
  };

  label = (color: LabelColor | null) => {
    const ids = this.targets();
    const cur = this.list.lc[this.index.get(ids[0]) ?? -1];
    const name = color ? LABELS.find((l) => l.color === color)!.name : null;
    const value = ids.length === 1 && cur === color ? null : name;
    this.patch(ids, { label: value }, ids.length > 1 ? `Labelled ${ids.length} photos` : undefined);
    if (this.settings.advanceAfterRate && ids.length === 1 && !this.compare) this.go(1);
  };

  togglePick = (v = 1) => {
    const ids = this.targets();
    const cur = this.list.p[this.index.get(ids[0]) ?? -1];
    const value = ids.length === 1 && cur === v ? 0 : v;
    this.patch(ids, { pick: value }, ids.length > 1 ? `${value ? 'Picked' : 'Unflagged'} ${ids.length} photos` : undefined);
    if (this.settings.advanceAfterRate && ids.length === 1 && value && !this.compare) this.go(1);
  };

  rotate = (deg: number) => {
    const ids = this.targets();
    this.patch(ids, { rotate: deg }).then(() => this.reload(true));
  };

  reject = async (idsIn?: number[]) => {
    const ids = idsIn ?? this.targets();
    if (!ids.length || this.readonlyGuard()) return;
    const inRejectFolder = ids.every((id) => this.folders.get(this.list.f[this.index.get(id) ?? -1])?.isReject);
    if (inRejectFolder) return this.restore(ids);
    this.busy++;
    try {
      const r = await api.reject(ids);
      this.afterMove(r, ids, `Moved to ${this.status?.reject_dir ?? '_Rejected'}`);
    } catch (e) {
      this.toast(`Not moved: ${(e as Error).message}`, 'error', undefined, 9000);
    } finally {
      this.busy--;
    }
  };

  restore = async (idsIn?: number[]) => {
    const ids = idsIn ?? this.targets();
    if (!ids.length || this.readonlyGuard()) return;
    this.busy++;
    try {
      const r = await api.restore(ids);
      this.afterMove(r, ids, 'Restored');
    } catch (e) {
      this.toast(`Not moved: ${(e as Error).message}`, 'error', undefined, 9000);
    } finally {
      this.busy--;
    }
  };

  private afterMove(r: BatchResult, ids: number[], verb: string) {
    if (r.failed.length) {
      this.toast(`${r.failed.length} not moved: ${r.failed[0][1]}`, 'error', undefined, 9000);
    }
    if (r.ok.length) {
      this.toast(`${verb}: ${r.ok.length} photo${r.ok.length > 1 ? 's' : ''}`, 'ok', { label: 'Undo', run: () => this.undo(r.batch) });
      for (const id of r.ok) this.selected.delete(id);
      // Drop moved photos from the current list immediately (they changed folder).
      const keep = (i: number) => !r.ok.includes(this.list.id[i]);
      const stay = this.filters.showRejected && this.recursive;
      if (!stay) {
        const L = this.list;
        const cols = Object.keys(L) as (keyof PhotoList)[];
        const next = {} as PhotoList;
        const idxs = L.id.map((_, i) => i).filter(keep);
        for (const c of cols) (next as any)[c] = idxs.map((i) => (L as any)[c][i]);
        const oldCurrent = this.current;
        const oldCursor = this.cursor;
        this.list = next;
        const idx = new Map<number, number>();
        next.id.forEach((id, i) => idx.set(id, i));
        this.index = idx;
        const removedBefore = L.id.slice(0, oldCurrent).filter((id) => r.ok.includes(id)).length;
        this.current = Math.min(Math.max(0, oldCurrent - removedBefore), Math.max(0, next.id.length - 1));
        const cursorBefore = L.id.slice(0, oldCursor).filter((id) => r.ok.includes(id)).length;
        this.cursor = Math.min(Math.max(0, oldCursor - cursorBefore), Math.max(0, next.id.length - 1));
        if (!this.settings.advanceAfterReject && this.current > 0 && ids.length === 1) this.current--;
        if (next.id.length === 0) this.loupe = false;
        this.pruneCompare();
        this.rev++;
        if (this.loupe || this.compare) this.loadDetail();
      } else {
        this.reload(true);
      }
      this.loadTree();
    }
  }

  undo = async (batch?: string) => {
    if (this.readonlyGuard()) return;
    this.busy++;
    try {
      const r = await api.undo(batch);
      if (r.job) {
        // A large change undoes in the background; its progress shows in the header.
        this.onJob(r.job);
        this.toast('Undoing in the background…', 'info', undefined, 3000);
        return;
      }
      if (r.failed.length) this.toast(`Undo incomplete: ${r.failed[0][1]}`, 'error', undefined, 8000);
      else this.toast('Undone', 'ok');
      await this.reload(true);
      this.loadTree();
      if (this.loupe) this.loadDetail();
    } catch (e) {
      this.toast((e as Error).message, 'error');
    } finally {
      this.busy--;
    }
  };

  openLocation = (ids?: number[]) => {
    const t = ids ?? this.targets();
    if (!t.length) {
      this.toast('Select photos first', 'info');
      return;
    }
    this.locationTargets = t;
    this.dialog = 'location';
  };

  // ------------------------------------------------------------ viewer toggles
  setViewer = <K extends keyof ViewerState>(k: K, v: ViewerState[K]) => {
    this.viewer[k] = v;
    this.persist();
  };

  /** Steps the exposure correction (turning it on), within ±5 EV. */
  bumpEv = (d: number) => {
    this.viewer.ev = Math.max(-5, Math.min(5, +(this.viewer.ev + d).toFixed(2)));
    this.viewer.evOn = true;
    this.persist();
  };

  toggleTone = (t: ToneMode) => {
    const next = this.viewer.tone === t ? 'normal' : t;
    this.viewer.tone = next;
    if (next !== 'normal' && this.viewer.source !== 'raw') {
      this.viewer.source = 'raw';
      this.toast('Switched to RAW view', 'info', undefined, 1800);
    }
    this.persist();
  };
}

export const store = new Store();

export function fmtExposure(t: number | null | undefined): string {
  if (!t) return '';
  if (t >= 1) return `${Number(t.toFixed(1))}s`;
  return `1/${Math.round(1 / t)}`;
}

export function fmtSettings(d: { exposure?: number | null; fnum?: number | null; iso?: number | null; focal?: number | null; bias?: number | null }, focal = true): string {
  const parts: string[] = [];
  if (d.exposure) parts.push(fmtExposure(d.exposure));
  if (d.fnum) parts.push(`f/${Number(d.fnum.toFixed(1))}`);
  if (d.iso) parts.push(`ISO ${d.iso}`);
  if (focal && d.focal) parts.push(`${Math.round(d.focal)} mm`);
  if (d.bias) parts.push(`${d.bias > 0 ? '+' : ''}${Number(d.bias.toFixed(2))} EV`);
  return parts.join(' · ');
}

export function fmtSize(b: number): string {
  if (b >= 1e9) return `${(b / 1e9).toFixed(2)} GB`;
  if (b >= 1e6) return `${(b / 1e6).toFixed(1)} MB`;
  if (b >= 1e3) return `${Math.round(b / 1e3)} KB`;
  return `${b} B`;
}

export function fmtDate(s: string | null | undefined, withTime = true): string {
  if (!s) return '';
  const m = s.match(/^(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}):(\d{2})(?::(\d{2}))?)?/);
  if (!m) return s;
  const d = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +(m[4] ?? 0), +(m[5] ?? 0), +(m[6] ?? 0)));
  const date = d.toLocaleDateString(undefined, { timeZone: 'UTC', weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' });
  if (!withTime || m[4] == null) return date;
  return `${date} · ${d.toLocaleTimeString(undefined, { timeZone: 'UTC', hour: '2-digit', minute: '2-digit', second: '2-digit' })}`;
}

export function cameraName(make: string | null, model: string | null): string {
  if (!model) return make ?? '';
  if (!make) return model;
  const m = make.split(' ')[0].toLowerCase();
  return model.toLowerCase().startsWith(m) ? model : `${make} ${model}`;
}
