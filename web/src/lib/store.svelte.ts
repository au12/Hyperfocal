import { SvelteSet } from 'svelte/reactivity';
import {
  api,
  emptyList,
  urls,
  type BatchResult,
  type Folder,
  type FolderRow,
  type LabelColor,
  type Patch,
  type PhotoDetail,
  type PhotoList,
  type Status,
  type ToneMode,
  type WbMode,
} from './api';

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
  sharpen1: number;
  sharpen2: number;
  peakColor: string;
  overColor: 'channels' | 'red';
  underColor: string;
  bgGray: boolean;
}

const DEFAULT_SETTINGS: Settings = {
  theme: 'dark',
  defaultSource: 'jpeg',
  prefetch: 3,
  keymap: 'frv',
  rowHeight: 200,
  showNames: false,
  advanceAfterRate: false,
  advanceAfterReject: true,
  sharpen1: 0.55,
  sharpen2: 1.2,
  peakColor: '#ff3b30',
  overColor: 'channels',
  underColor: '#2f6fff',
  bgGray: false,
};

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
  view = $state<'grid' | 'map'>('grid');
  filters = $state<Filters>(
    loadLocal('hf.filters', {
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
    } as Filters),
  );

  // Photo list (columnar, not deeply reactive; `rev` signals in-place changes)
  list = $state.raw<PhotoList>(emptyList());
  rev = $state(0);
  loading = $state(false);
  listError = $state<string | null>(null);
  private listAbort: AbortController | null = null;
  index = new Map<number, number>();

  selected = new SvelteSet<number>();
  cursor = $state(0);
  anchor = 0;

  // Single view
  loupe = $state(false);
  current = $state(0);
  detail = $state<PhotoDetail | null>(null);
  private detailAbort: AbortController | null = null;
  viewer = $state<ViewerState>(
    loadLocal('hf.viewer', { source: 'jpeg', tone: 'normal', wb: 'asshot', ev: 0, evOn: true, sharpen: 0, peaking: 0, over: false, under: false } as ViewerState),
  );
  panels = $state(loadLocal('hf.panels', { sidebar: true, info: true, histogram: false, filmstrip: true, hidden: false }));

  settings = $state<Settings>(loadLocal('hf.settings', DEFAULT_SETTINGS));
  toasts = $state<Toast[]>([]);
  dialog = $state<null | 'settings' | 'help' | 'journal' | 'location' | 'keywords'>(null);
  locationTargets = $state.raw<number[]>([]);
  mobile = $state(false);
  busy = $state(0);

  private es: EventSource | null = null;
  private reloadTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    if (typeof window !== 'undefined') {
      const mq = matchMedia('(max-width: 760px)');
      this.mobile = mq.matches;
      mq.addEventListener('change', (e) => (this.mobile = e.matches));
      if (!localStorage.getItem('hf.viewer')) this.viewer.source = this.settings.defaultSource;
      window.addEventListener('hf-auth', () => (this.needLogin = true));
    }
  }

  persist = () => {
    saveLocal('hf.settings', this.settings);
    saveLocal('hf.viewer', this.viewer);
    saveLocal('hf.panels', this.panels);
    saveLocal('hf.filters', this.filters);
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
    try {
      this.status = await api.status();
      if (this.status.auth && !this.status.logged_in) {
        this.needLogin = true;
        return;
      }
    } catch (e) {
      this.toast(`Server unreachable: ${(e as Error).message}`, 'error');
      return;
    }
    await this.loadTree();
    this.connectEvents();
    const hash = new URLSearchParams(location.hash.slice(1));
    const f = Number(hash.get('f'));
    const start = this.folders.has(f) ? f : (this.lastFolder() ?? this.roots[0]?.id ?? null);
    if (start != null) await this.openFolder(start, false);
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
    if (this.folderId != null) h.set('f', String(this.folderId));
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
      } else if (e.type === 'folder') {
        if (this.affects(e.id)) this.scheduleReload();
        this.scheduleTree();
      } else if (e.type === 'photos') {
        const ids: number[] = e.ids ?? [];
        if (ids.some((i) => this.index.has(i))) this.scheduleReload();
        if (this.detail && ids.includes(this.detail.id)) this.loadDetail();
      }
    };
    es.onerror = () => {
      /* EventSource reconnects by itself */
    };
  };

  private treeTimer: ReturnType<typeof setTimeout> | null = null;
  private scheduleTree() {
    if (this.treeTimer) clearTimeout(this.treeTimer);
    this.treeTimer = setTimeout(() => this.loadTree(), 1500);
  }

  private affects(folderId: number): boolean {
    if (this.query) return true;
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
    if (fromUser) this.query = '';
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
    this.selected.clear();
    await this.reload();
    this.cursor = 0;
    this.writeHash();
  };

  params = (): Record<string, unknown> => {
    const f = this.filters;
    const searching = this.query.trim().length > 0;
    const root = this.roots[0]?.id;
    return {
      folder: searching ? root : (this.folderId ?? root),
      rec: searching ? 1 : this.recursive,
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
      q: searching ? this.query.trim() : undefined,
      sort: f.sort === 'taken' ? undefined : f.sort,
      desc: f.desc,
    };
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
    this.reload();
  };

  // ------------------------------------------------------------ selection
  targets = (): number[] => {
    if (this.loupe) {
      const id = this.list.id[this.current];
      return id != null ? [id] : [];
    }
    if (this.selected.size > 0) return [...this.selected];
    const id = this.list.id[this.cursor];
    return id != null ? [id] : [];
  };

  select = (i: number, mode: 'single' | 'toggle' | 'range') => {
    const id = this.list.id[i];
    if (id == null) return;
    if (mode === 'single') {
      this.selected.clear();
      this.selected.add(id);
      this.anchor = i;
    } else if (mode === 'toggle') {
      if (this.selected.has(id)) this.selected.delete(id);
      else this.selected.add(id);
      this.anchor = i;
    } else {
      const [a, b] = this.anchor < i ? [this.anchor, i] : [i, this.anchor];
      for (let k = a; k <= b; k++) this.selected.add(this.list.id[k]);
    }
    this.cursor = i;
  };

  selectAll = () => {
    for (const id of this.list.id) this.selected.add(id);
  };
  clearSelection = () => this.selected.clear();
  invertSelection = () => {
    for (const id of this.list.id) {
      if (this.selected.has(id)) this.selected.delete(id);
      else this.selected.add(id);
    }
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

  go = (delta: number) => {
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
      this.cursor = i;
    }
  };

  goTo = (i: number) => this.go(i - (this.loupe ? this.current : this.cursor));

  loadDetail = async () => {
    const id = this.list.id[this.loupe ? this.current : this.cursor];
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

  /** Detail for the photo the info panel should show (grid cursor or loupe). */
  focusId = (): number | undefined => this.list.id[this.loupe ? this.current : this.cursor];

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
    if (this.settings.advanceAfterRate && ids.length === 1) this.go(1);
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
    if (this.settings.advanceAfterRate && ids.length === 1) this.go(1);
  };

  togglePick = (v = 1) => {
    const ids = this.targets();
    const cur = this.list.p[this.index.get(ids[0]) ?? -1];
    const value = ids.length === 1 && cur === v ? 0 : v;
    this.patch(ids, { pick: value }, ids.length > 1 ? `${value ? 'Picked' : 'Unflagged'} ${ids.length} photos` : undefined);
    if (this.settings.advanceAfterRate && ids.length === 1 && value) this.go(1);
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
        this.list = next;
        const idx = new Map<number, number>();
        next.id.forEach((id, i) => idx.set(id, i));
        this.index = idx;
        const removedBefore = L.id.slice(0, oldCurrent).filter((id) => r.ok.includes(id)).length;
        this.current = Math.min(Math.max(0, oldCurrent - removedBefore), Math.max(0, next.id.length - 1));
        this.cursor = Math.min(this.cursor, Math.max(0, next.id.length - 1));
        if (!this.settings.advanceAfterReject && this.current > 0 && ids.length === 1) this.current--;
        if (next.id.length === 0) this.loupe = false;
        this.rev++;
        if (this.loupe) this.loadDetail();
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
