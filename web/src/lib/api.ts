// Typed client for the Hyperfocal API. All URLs are relative so the app works behind
// a reverse proxy, on a sub-path or straight on the container port.

export type ToneMode = 'normal' | 'linear' | 'shadows' | 'highlights';
export type WbMode = 'asshot' | 'daylight' | 'auto';
export type LabelColor = 'red' | 'yellow' | 'green' | 'blue' | 'purple';

export interface Progress {
  phase: string;
  folders_total: number;
  folders_done: number;
  photos_indexed: number;
  current: string | null;
  last_full_scan: number | null;
  thumbs_pending: number;
  library_ok: boolean;
  message: string | null;
}

export interface Status {
  version: string;
  readonly: boolean;
  reject_dir: string;
  progress: Progress;
  counts: { photos: number; folders: number; geotagged: number; integrity?: IntegrityCounts };
  cache: Record<string, number>;
  decoders?: { flashraw: string; libraw: string | null; libheif: string | null; raw: 'flashraw' | 'libraw' };
  /** The archive integrity monitor runs. */
  integrity?: boolean;
  auth: boolean;
  logged_in: boolean;
}

/** Folder tuple from /api/tree: [id, parent, name, count, isReject, scanned] */
export type FolderRow = [number, number | null, string, number, number, boolean];

export interface Folder {
  id: number;
  parent: number | null;
  name: string;
  count: number;
  isReject: boolean;
  children: Folder[];
  total: number;
  path: string;
  depth: number;
}

/** Columnar photo list. */
export interface PhotoList {
  id: number[];
  n: string[];
  w: number[];
  h: number[];
  o: number[];
  t: (number | null)[];
  r: number[];
  lc: (LabelColor | null)[];
  p: number[];
  k: number[];
  f: number[];
  g: number[];
  v: string[];
  x: string[];
}

export const emptyList = (): PhotoList => ({ id: [], n: [], w: [], h: [], o: [], t: [], r: [], lc: [], p: [], k: [], f: [], g: [], v: [], x: [] });

export interface PhotoDetail {
  id: number;
  folder_id: number;
  folder: string;
  in_reject: boolean;
  name: string;
  ext: string;
  kind: number;
  size: number;
  mtime: number;
  width: number | null;
  height: number | null;
  orientation: number;
  taken: number | null;
  taken_str: string | null;
  make: string | null;
  model: string | null;
  lens: string | null;
  focal: number | null;
  fnum: number | null;
  exposure: number | null;
  iso: number | null;
  bias: number | null;
  rating: number;
  label: string | null;
  lcolor: LabelColor | null;
  pick: number;
  title: string | null;
  description: string | null;
  creator: string | null;
  rights: string | null;
  keywords: string[];
  hkeywords: string[];
  lat: number | null;
  lon: number | null;
  alt: number | null;
  city: string | null;
  state: string | null;
  country: string | null;
  country_code: string | null;
  sublocation: string | null;
  sidecar: string | null;
  sidecar_size: number | null;
  companion: string | null;
  files: string[];
  error: string | null;
  v: string;
}

export interface RawInfo {
  w: number;
  h: number;
  hist: number[][];
  hist_min_ev: number;
  hist_per_ev: number;
  /** Fractions of clipped / underexposed samples per channel (R, G, B), then of pixels with any. */
  over: [number, number, number, number];
  under: [number, number, number, number];
  wb: [number, number, number];
  baseline_ev: number;
  boost_ev: number;
  decode_ms: number;
  develop_ms: number;
  approximate: boolean;
  /** Which decoder read the sensor data: "FlashRaw" or "LibRaw". */
  decoder?: string;
  /** Full-resolution view: size of the demosaiced frame (absent for non-Bayer sensors),
   * the tile grid, and the pixels each tile repeats from its neighbours on each side. */
  full?: [number, number] | null;
  tile?: number;
  apron?: number;
}

export interface Patch {
  rating?: number;
  label?: string | null;
  pick?: number;
  title?: string | null;
  description?: string | null;
  creator?: string | null;
  rights?: string | null;
  keywords_add?: string[];
  keywords_remove?: string[];
  keywords_set?: string[];
  gps?: { lat: number; lon: number; alt?: number | null } | null;
  city?: string | null;
  state?: string | null;
  country?: string | null;
  country_code?: string | null;
  sublocation?: string | null;
  orientation?: number;
  rotate?: number;
}

export interface BatchResult {
  batch: string;
  ok: number[];
  failed: [number, string][];
  /** Set when the change runs in the background. */
  job?: JobStatus;
}

/** A change written in the background (keyword moves, GPS from a track, large undos). */
export interface JobStatus {
  id: string;
  label: string;
  total: number;
  done: number;
  failed: number;
  finished: boolean;
  /** Undoing this batch undoes the whole job. */
  batch: string;
  errors: [number, string][];
  started: number;
}

export interface JournalEntry {
  batch: string;
  ts: number;
  kind: string;
  count: number;
  summary: string | null;
  undone: boolean;
  undo_of: string | null;
}

export interface Cluster {
  n: number;
  lat: number;
  lon: number;
  id: number;
  v: string | null;
  bounds: [number, number, number, number];
}

export interface Facets {
  cameras: [string, number][];
  lenses: [string, number][];
  years: [string, number][];
  labels: [string, number][];
}

/** A timeline scope: capture times from `from` (inclusive) to `to` (exclusive), in
 * seconds on the camera's clock; or the photos without a capture time. */
export interface DateRange {
  from: number;
  to: number;
  label: string;
  nodate?: boolean;
}

/** What a saved collection shows. Absent fields mean "as default". */
export interface CollectionSpec {
  q?: string;
  /** Folder path ('' = the whole library). */
  folder?: string;
  rec?: boolean;
  filters?: Record<string, unknown>;
  date?: DateRange | null;
}

export interface Collection {
  id: number;
  name: string;
  spec: CollectionSpec;
  created?: number;
  updated?: number;
}

export interface Timeline {
  /** [day since 1970-01-01, photos] */
  days: [number, number][];
  undated: number;
}

export interface IntegrityCounts {
  changed: number;
  missing: number;
  unreadable: number;
}

/** 1 changed, 2 missing, 3 unreadable. */
export interface IntegrityProblem {
  id: number;
  folder: string;
  name: string;
  status: 1 | 2 | 3;
  since: number | null;
  detail: string | null;
  size: number;
  verified_at: number;
  first_seen: number;
  notified: boolean;
  folder_id: number | null;
  photo_id: number | null;
}

export interface IntegritySession {
  started: number;
  ended: number | null;
  files: number;
  bytes: number;
  problems: number;
  manual: boolean;
}

export interface IntegritySummary extends IntegrityCounts {
  enabled: boolean;
  originals: number;
  indexed: number;
  bytes: number;
  verified_recently: number;
  oldest_verified: number | null;
  problems: IntegrityProblem[];
  worker: { phase: string; current: string | null; session: IntegritySession | null; last_alert: number | null; alert_error: string | null };
  window: { open: boolean; next: number; hour: number; minutes: number };
  days: number;
  mbps: number;
  alert: { host: string; format: string } | null;
}

/** A track file loaded for geotagging (kept by the server for a few hours). */
export interface TrackInfo {
  id: string;
  name: string;
  format: string;
  points: number;
  /** First and last fix, ms since the epoch (UTC). */
  start: number;
  end: number;
}

export interface GeotagParams {
  tracks: string[];
  ids: number[];
  /** The camera's time zone, minutes east of UTC; null: find it. */
  tz: number | null;
  /** Seconds added to the camera's clock. */
  drift: number;
  /** Longest gap between fixes a photo may fall into, seconds. */
  gap: number;
  replace: boolean;
  /** The browser's zone, preferred among zones that fit as well. */
  prefer: number;
}

export interface GeotagPreview {
  tz: number;
  detected: boolean;
  plateau: [number, number] | null;
  counts: [number, number][];
  matched: number;
  own_zone: number;
  no_time: number;
  outside: number;
  has_gps: number;
  total: number;
  /** [id, lat, lon] of every photo that falls on the track. */
  photos: [number, number, number][];
  /** The track for drawing: lines of [lat, lon]. */
  track: [number, number][][];
  range: [number, number];
  /** First and last moment of the photos considered (UTC ms). */
  span: [number, number] | null;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

const BASE = new URL('./api/', document.baseURI).pathname;

export function apiUrl(path: string, params?: Record<string, unknown>): string {
  let u = BASE + path.replace(/^\//, '');
  if (params) {
    const q = new URLSearchParams();
    for (const [k, v] of Object.entries(params)) {
      if (v === undefined || v === null || v === '' || v === false) continue;
      q.set(k, v === true ? '1' : String(v));
    }
    const s = q.toString();
    if (s) u += '?' + s;
  }
  return u;
}

async function req<T>(method: string, path: string, body?: unknown, params?: Record<string, unknown>, signal?: AbortSignal): Promise<T> {
  const res = await fetch(apiUrl(path, params), {
    method,
    headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
    signal,
  });
  if (!res.ok) {
    let msg = res.statusText;
    try {
      const j = await res.json();
      msg = j.error ?? msg;
    } catch {
      /* not json */
    }
    if (res.status === 401) window.dispatchEvent(new CustomEvent('hf-auth'));
    throw new ApiError(res.status, msg);
  }
  return res.json() as Promise<T>;
}

export const api = {
  status: () => req<Status>('GET', 'status'),
  tree: () => req<{ folders: FolderRow[] }>('GET', 'tree'),
  photos: (params: Record<string, unknown>, signal?: AbortSignal) => req<PhotoList>('GET', 'photos', undefined, params, signal),
  count: (params: Record<string, unknown>, signal?: AbortSignal) => req<{ count: number }>('GET', 'photos', undefined, { ...params, count: 1 }, signal),
  timeline: () => req<Timeline>('GET', 'timeline'),
  collections: () => req<Collection[]>('GET', 'collections'),
  saveCollection: (c: { id?: number; name: string; spec: CollectionSpec }) => req<Collection>('POST', 'collections', c),
  deleteCollection: (id: number) => req<{ ok: boolean }>('DELETE', `collections/${id}`),
  photo: (id: number, signal?: AbortSignal) => req<PhotoDetail>('GET', `photo/${id}`, undefined, undefined, signal),
  rawinfo: (id: number, v: string, wb: WbMode, signal?: AbortSignal) => req<RawInfo>('GET', `photo/${id}/rawinfo`, undefined, { v, wb }, signal),
  facets: () => req<Facets>('GET', 'facets'),
  keywords: (q: string, signal?: AbortSignal) => req<[string, number][]>('GET', 'keywords', undefined, { q }, signal),
  keywordTree: () => req<{ nodes: [string, number][] }>('GET', 'keywords/tree'),
  moveKeyword: (from: string, to: string | null) => req<{ job: JobStatus }>('POST', 'keywords/move', { from, to }),
  jobs: () => req<JobStatus[]>('GET', 'jobs'),
  clusters: (params: Record<string, unknown>, signal?: AbortSignal) => req<{ cell: number; clusters: Cluster[] }>('GET', 'geo/clusters', undefined, params, signal),
  meta: (ids: number[], patch: Patch) => req<BatchResult>('POST', 'meta', { ids, patch }),
  reject: (ids: number[]) => req<BatchResult>('POST', 'reject', { ids }),
  restore: (ids: number[]) => req<BatchResult>('POST', 'restore', { ids }),
  undo: (batch?: string) => req<BatchResult>('POST', 'undo', { batch }),
  journal: () => req<JournalEntry[]>('GET', 'journal'),
  rescan: (folder?: number) => req<{ ok: boolean }>('POST', 'rescan', { folder }),
  settings: () => req<Record<string, unknown>>('GET', 'settings'),
  saveSettings: (s: Record<string, unknown>) => req<Record<string, unknown>>('POST', 'settings', s),
  uploadTrack: (file: File, onProgress?: (fraction: number) => void) =>
    new Promise<TrackInfo>((resolve, reject) => {
      // XMLHttpRequest, for upload progress: a location history can be hundreds of MB.
      const x = new XMLHttpRequest();
      x.open('POST', apiUrl('geotag/tracks', { name: file.name }));
      x.withCredentials = true;
      x.setRequestHeader('content-type', 'application/octet-stream');
      x.upload.onprogress = (e) => {
        if (e.lengthComputable) onProgress?.(e.loaded / e.total);
      };
      x.onload = () => {
        let j: any = null;
        try {
          j = JSON.parse(x.responseText);
        } catch {
          /* not json */
        }
        if (x.status >= 200 && x.status < 300 && j) resolve(j as TrackInfo);
        else {
          if (x.status === 401) window.dispatchEvent(new CustomEvent('hf-auth'));
          reject(new ApiError(x.status, j?.error ?? (x.statusText || 'upload failed')));
        }
      };
      x.onerror = () => reject(new ApiError(0, 'the upload failed (network error)'));
      x.send(file);
    }),
  forgetTrack: (id: string) => req<{ ok: boolean }>('DELETE', `geotag/tracks/${encodeURIComponent(id)}`),
  geotagPreview: (p: GeotagParams, signal?: AbortSignal) => req<GeotagPreview>('POST', 'geotag/preview', p, undefined, signal),
  geotagApply: (p: GeotagParams) => req<{ job: JobStatus }>('POST', 'geotag/apply', p),
  integrity: () => req<IntegritySummary>('GET', 'integrity'),
  integrityRecheck: (ids: number[]) => req<{ ok: boolean }>('POST', 'integrity/recheck', { ids }),
  integrityAccept: (ids: number[]) => req<{ accepted: number }>('POST', 'integrity/accept', { ids }),
  integrityForget: (ids: number[]) => req<{ forgotten: number }>('POST', 'integrity/forget', { ids }),
  integrityRun: (o: { all?: boolean; stop?: boolean }) => req<{ ok: boolean }>('POST', 'integrity/run', o),
  integrityTestAlert: () => req<{ ok: boolean }>('POST', 'integrity/test-alert', {}),
  login: (password: string) => req<{ ok: boolean }>('POST', 'login', { password }),
  logout: () => req<{ ok: boolean }>('POST', 'logout', {}),
};

export const urls = {
  /** `r`: bumped when a draft thumbnail (shown at once for a HEIC file) was replaced. */
  thumb: (id: number, v: string, r?: number) => apiUrl(`photo/${id}/thumb`, { v, r: r || undefined }),
  preview: (id: number, v: string, w: number) => apiUrl(`photo/${id}/preview`, { v, w }),
  jpeg: (id: number, v: string) => apiUrl(`photo/${id}/jpeg`, { v }),
  raw: (id: number, v: string, o: { w: number; mode: ToneMode; ev: number; wb: WbMode; low?: boolean }) =>
    apiUrl(`photo/${id}/raw`, { v, w: o.w, mode: o.mode === 'normal' ? undefined : o.mode, ev: o.ev ? o.ev.toFixed(2) : undefined, wb: o.wb === 'asshot' ? undefined : o.wb, prio: o.low ? 'low' : undefined }),
  rawmask: (id: number, v: string, wb: WbMode) => apiUrl(`photo/${id}/rawmask`, { v, wb: wb === 'asshot' ? undefined : wb }),
  /** A full-resolution tile of the RAW view, demosaiced and tone-mapped on the server (JPEG). */
  rawtile: (id: number, v: string, o: { mode: ToneMode; ev: number; wb: WbMode; tx: number; ty: number }) =>
    apiUrl(`photo/${id}/rawtile`, { v, mode: o.mode === 'normal' ? undefined : o.mode, ev: o.ev ? o.ev.toFixed(2) : undefined, wb: o.wb === 'asshot' ? undefined : o.wb, tx: o.tx, ty: o.ty }),
  /** A tile's sensor samples, for demosaicing on this device's GPU. */
  cfatile: (id: number, v: string, wb: WbMode, tx: number, ty: number) => apiUrl(`photo/${id}/cfatile`, { v, wb: wb === 'asshot' ? undefined : wb, tx, ty }),
  /** The tone curve of a RAW view (65536 bytes), for the GPU path. */
  lut: (mode: ToneMode, ev: number, ver: string) => apiUrl('lut', { mode: mode === 'normal' ? undefined : mode, ev: ev ? ev.toFixed(2) : undefined, v: ver || undefined }),
  events: () => apiUrl('events'),
};
