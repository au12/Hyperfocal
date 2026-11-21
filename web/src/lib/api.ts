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
  /** Send to Immich is set up: Immich's address, and whether it reads this library. */
  immich?: { url: string; library: boolean } | null;
  /** Search by content: whether it can run, and how many photos it has looked at. */
  visual?: { enabled: boolean; phase: string; done: number; total: number; error: string | null };
  /** Culling's measurements: how far along, and whether eyes are checked. */
  cull?: CullStatus;
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
  /** Culling (with cull=1): issue bits (1 blurry, 2 eyes closed, 4 highlights clipped,
   * 8 too dark; -1 not measured yet) and the subject's sharpness (0..100, -1 unknown). */
  cq?: number[];
  cf?: number[];
  /** Series (with stack=1 or weak=1): the stack's key (its first frame; 0: none), how many
   * frames it has, and 1 for its best frame. */
  sk?: number[];
  sn?: number[];
  sb?: number[];
}

export const ISSUE_BLUR = 1;
export const ISSUE_EYES = 2;
export const ISSUE_OVER = 4;
export const ISSUE_UNDER = 8;

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
  /** The albums it is in: [id, name]. */
  albums?: [number, string][];
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

/** The zoom pyramid of a photo's non-RAW view: full size (in the file's orientation),
 * levels (each half the previous; 0 when previews are as sharp), tile size and apron. */
export interface Pyramid {
  w: number;
  h: number;
  levels: number;
  tile: number;
  apron: number;
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
  /** Undoing the batch undoes the job (false: it changed nothing here). */
  undo?: boolean;
  /** What it did, once finished. */
  summary?: string;
  /** Where to see the result. */
  link?: string;
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

/** An album on the Immich server. */
export interface ImmichAlbum {
  id: string;
  name: string;
  count: number;
  thumb: string | null;
}

/** A hand-picked set of photos in an order of its own. */
export interface Album {
  id: number;
  name: string;
  note: string | null;
  position: number;
  created: number;
  updated: number;
  /** Photos shown (reject folders left out). */
  count: number;
  /** Members whose file left the library (found again if it turns up). */
  missing: number;
  /** The cover: the photo set as cover, or the first one. */
  cover: { id: number; v: string } | null;
  cover_set: number | null;
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

export interface CullStatus {
  phase: 'starting' | 'off' | 'waiting' | 'measuring' | 'idle' | 'offline' | string;
  done: number;
  total: number;
  rate: number;
  error: string | null;
  eyes: boolean;
}

/** A face as culling sees it: where it is (upright photo, 0..1), how sharp (0..1), eyes
 * (0 open .. 1 closed; `shut` past the threshold), who. */
export interface CullFace {
  id: number;
  photo: number;
  box: [number, number, number, number];
  focus: number | null;
  closed: number | null;
  shut: boolean | null;
  eyes: [[number, number], [number, number]] | null;
  subject: { id: number; name: string | null } | null;
  /** Version for the crop URL. */
  v: string;
}

export interface CullFrame {
  id: number;
  name: string;
  v: string;
  w: number | null;
  h: number | null;
  o: number;
  rating: number;
  pick: number;
  focus: number | null;
  closed: number | null;
  issues: CullIssue[];
}

export type CullIssue = 'blur' | 'eyes' | 'over' | 'under';

export interface CullPhoto {
  id: number;
  measured: boolean;
  pending: boolean;
  error: string | null;
  focus: number | null;
  focus_on: 'faces' | 'animals' | 'photo' | null;
  over: number | null;
  under: number | null;
  eyes: { checked: number; closed: number | null } | null;
  issues: CullIssue[];
  faces: CullFace[];
  /** The series it belongs to: its frames in shooting order, the best one and why, and
   * each face of this photo followed through the frames. */
  series: {
    key: number;
    best: number;
    why: string | null;
    index: number;
    frames: CullFrame[];
    tracks: { subject: { id: number; name: string | null } | null; faces: (CullFace | null)[] }[];
  } | null;
}

export type DupeMode = 'exact' | 'near' | 'series' | 'visual';
export type DupeScope = 'library' | 'folder' | 'against';
export type DupeLevel = 'strict' | 'normal' | 'loose';

export interface DupeMember {
  id: number;
  name: string;
  /** Folder path ('' for the library root). */
  folder: string;
  folder_id: number;
  /** Thumbnail version. */
  v: string;
  /** File-type badge: cr3, cr3+jpg. */
  label: string;
  kind: number;
  w: number | null;
  h: number | null;
  orientation: number;
  /** Bytes, the JPEG/HEIF twin included. */
  bytes: number;
  taken: number | null;
  rating: number;
  lcolor: string | null;
  pick: number;
  /** How much metadata it carries (0-7). */
  meta: number;
  /** Perceptual-hash bits that differ from the keeper's (of 64). */
  d?: number;
  /** How alike it looks to the keeper, percent (look-alikes). */
  alike?: number;
  /** A turned or mirrored copy of the keeper: how. */
  turned?: string;
  /** In the folder (folder against the library). */
  inside: boolean;
}

export interface DupeGroup {
  key: string;
  /** The suggested keeper first. */
  members: DupeMember[];
  keeper: number;
  reason: string;
  extra_bytes: number;
}

export interface DupesStatus {
  enabled: boolean;
  /** off, waiting, hashing, idle */
  phase: string;
  pending: number;
  done: number;
  current: string | null;
}

export interface DupeReport {
  groups: DupeGroup[];
  total_groups: number;
  photos: number;
  extra: number;
  extra_bytes: number;
  truncated: boolean;
  covered: number;
  eligible: number;
  unreadable: number;
  dismissed: number;
  status: DupesStatus;
  /** The integrity monitor is on (exact copies need its hashes). */
  integrity: boolean;
  ms: number;
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

// ---------------------------------------------------------------- people & pets

export type SubjectKind = 'person' | 'dog' | 'cat';
export type DetKind = 'face' | 'dog' | 'cat';

/** A face, dog or cat found in a photo. `box` is [x, y, w, h] on the upright photo
 * (0..1); state 0: found or suggested, 1: confirmed; src 1: a region from another app. */
export interface Detection {
  id: number;
  photo: number;
  kind: DetKind;
  box: [number, number, number, number];
  q: number;
  state: 0 | 1;
  sim: number | null;
  src: 0 | 1;
  subject: number | null;
  score: number;
  /** Version for the crop URL. */
  v: string;
  who?: { id: number; name: string | null; kind: SubjectKind; hidden: boolean };
  /** In a subject's list: the photo's version, file name and capture time. */
  pv?: string;
  file?: string;
  taken?: number | null;
}

/** A person, a pet, or an unnamed group of either. */
export interface Subject {
  id: number;
  kind: SubjectKind;
  name: string | null;
  hidden: boolean;
  photos: number;
  confirmed: number;
  /** Suggestions waiting for a yes or no. */
  review: number;
  cover: Detection | null;
  keyword?: string;
}

export interface PeopleStatus {
  status: {
    enabled: boolean;
    phase: 'off' | 'connecting' | 'offline' | 'analysing' | 'idle' | 'paused' | 'grouping' | 'error' | string;
    service: { version: string; device: string; models: Record<string, { file: string }> } | null;
    error: string | null;
    analyzed: number;
    pending: number;
    failed: number;
    rate: number;
    current: string | null;
    regroup: number | null;
  };
  people: number;
  pets: number;
  groups: number;
  faces: number;
  animals: number;
  review: number;
  paused: boolean;
  /** Names are written to the sidecars. */
  xmp: boolean;
}

export interface PhotoPeople {
  analyzed: boolean;
  error: string | null;
  detections: Detection[];
  ignored: number;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

const BASE = new URL('./api/', document.baseURI).pathname;

/** Revision of the server's exposure analysis (clipping mask, stats). The mask and RAW
 * info are cached as immutable per photo version; bumping this makes browsers fetch them
 * again after the analysis changes. 2: FastRawViewer-calibrated thresholds (Nov 2026). */
const ANALYSIS_REV = 2;

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

/** Whether the last answer came from the offline copies (service worker) or the
 * server could not be reached; the store shows it. */
let offline = false;
function setNet(o: boolean) {
  if (o === offline) return;
  offline = o;
  window.dispatchEvent(new CustomEvent('hf-net', { detail: o }));
}

async function req<T>(method: string, path: string, body?: unknown, params?: Record<string, unknown>, signal?: AbortSignal): Promise<T> {
  let res: Response;
  try {
    res = await fetch(apiUrl(path, params), {
      method,
      headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
      body: body !== undefined ? JSON.stringify(body) : undefined,
      credentials: 'same-origin',
      signal,
    });
  } catch (e) {
    if (e instanceof TypeError) setNet(true);
    throw e;
  }
  setNet(res.headers.has('x-hf-offline'));
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
  albums: () => req<Album[]>('GET', 'albums'),
  createAlbum: (name: string, ids: number[] = []) => req<Album & { added: number[]; batch: string }>('POST', 'albums', { name, ids }),
  editAlbum: (id: number, b: { name?: string; note?: string | null; cover?: number | null }) => req<Album & { batch: string }>('POST', `albums/${id}`, b),
  deleteAlbum: (id: number) => req<{ ok: boolean; batch: string }>('DELETE', `albums/${id}`),
  albumAdd: (id: number, ids: number[], before?: number) => req<{ added: number[]; batch: string | null }>('POST', `albums/${id}/add`, { ids, before }),
  albumRemove: (id: number, ids: number[]) => req<{ removed: number[]; batch: string | null }>('POST', `albums/${id}/remove`, { ids }),
  albumOrder: (id: number, ids: number[], before: number | null) => req<{ moved: boolean; batch: string | null }>('POST', `albums/${id}/order`, { ids, before }),
  albumForget: (id: number) => req<{ forgotten: number; batch: string | null }>('POST', `albums/${id}/forget`, {}),
  orderAlbums: (ids: number[]) => req<{ ok: boolean }>('POST', 'albums/order', { ids }),
  immichAlbums: () => req<ImmichAlbum[]>('GET', 'immich/albums'),
  immichSend: (b: { ids: number[]; album?: string; album_name?: string; mode: 'jpeg' | 'original' }) => req<{ job: JobStatus }>('POST', 'immich/send', b),
  photo: (id: number, signal?: AbortSignal) => req<PhotoDetail>('GET', `photo/${id}`, undefined, undefined, signal),
  rawinfo: (id: number, v: string, wb: WbMode, signal?: AbortSignal) => req<RawInfo>('GET', `photo/${id}/rawinfo`, undefined, { v, wb, a: ANALYSIS_REV }, signal),
  pyramid: (id: number, v: string, signal?: AbortSignal) => req<Pyramid>('GET', `photo/${id}/pyramid`, undefined, { v }, signal),
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
  dupes: (p: { mode: DupeMode; scope: DupeScope; folder?: number; level?: DupeLevel; gap?: number; min?: number }, signal?: AbortSignal) =>
    req<DupeReport>('GET', 'dupes', undefined, p, signal),
  dupesStatus: () => req<DupesStatus>('GET', 'dupes/status'),
  dupesDismiss: (ids: number[], undo = false) => req<{ pairs: number }>('POST', 'dupes/dismiss', { ids, undo }),
  dupesClearDismissed: () => req<{ cleared: number }>('DELETE', 'dupes/dismissed'),
  peopleStatus: () => req<PeopleStatus>('GET', 'people/status'),
  subjects: (p: { kind: 'person' | 'pet'; hidden?: boolean; named?: 'named' | 'unnamed'; min?: number; q?: string; limit?: number }, signal?: AbortSignal) =>
    req<{ subjects: Subject[]; total: number }>('GET', 'people', undefined, { ...p, hidden: p.hidden ? 1 : undefined }, signal),
  subject: (id: number) => req<Subject>('GET', `people/${id}`),
  subjectDetections: (id: number, p: { state?: 'auto' | 'confirmed' | 'all'; limit?: number; offset?: number }, signal?: AbortSignal) =>
    req<Detection[]>('GET', `people/${id}/detections`, undefined, p, signal),
  updateSubject: (id: number, b: { name?: string | null; hidden?: boolean }) => req<{ ok: boolean }>('POST', `people/${id}`, b),
  deleteSubject: (id: number) => req<{ freed: number }>('DELETE', `people/${id}`),
  mergeSubjects: (from: number[], into: number) => req<{ subject: number; moved: number }>('POST', 'people/merge', { from, into }),
  assign: (ids: number[], who: { subject?: number; name?: string }) => req<{ subject: number }>('POST', 'people/assign', { ids, ...who }),
  confirm: (b: { ids?: number[]; subject?: number }) => req<{ confirmed: number }>('POST', 'people/confirm', b),
  rejectDetections: (ids: number[]) => req<{ rejected: number }>('POST', 'people/reject', { ids }),
  ignore: (ids: number[], ignored = true) => req<{ changed: number }>('POST', 'people/ignore', { ids, ignored }),
  peopleNames: (q: string, kind: DetKind | 'pet', signal?: AbortSignal) =>
    req<{ id: number; name: string; kind: SubjectKind; photos: number }[]>('GET', 'people/names', undefined, { q, kind }, signal),
  peopleAnalyze: (ids: number[]) => req<{ ok: boolean }>('POST', 'people/analyze', { ids }),
  peopleControl: (b: { pause?: boolean; regroup?: boolean; reanalyze?: number }) => req<{ ok: boolean; paused: boolean }>('POST', 'people/control', b),
  photoPeople: (id: number, signal?: AbortSignal) => req<PhotoPeople>('GET', `photo/${id}/people`, undefined, undefined, signal),
  cullPhoto: (id: number, signal?: AbortSignal) => req<CullPhoto>('GET', `cull/photo/${id}`, undefined, undefined, signal),
  cullStatus: () => req<CullStatus>('GET', 'cull/status'),
  cullMeasure: (ids: number[]) => req<{ ok: boolean }>('POST', 'cull/measure', { ids }),
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
  /** The RAW clipping mask; `ev`: the exposure correction the overexposure display follows. */
  rawmask: (id: number, v: string, wb: WbMode, ev = 0) =>
    apiUrl(`photo/${id}/rawmask`, { v, wb: wb === 'asshot' ? undefined : wb, ev: Math.abs(ev) >= 0.005 ? ev.toFixed(2) : undefined, a: ANALYSIS_REV }),
  /** A full-resolution tile of the RAW view, demosaiced and tone-mapped on the server (JPEG). */
  rawtile: (id: number, v: string, o: { mode: ToneMode; ev: number; wb: WbMode; tx: number; ty: number }) =>
    apiUrl(`photo/${id}/rawtile`, { v, mode: o.mode === 'normal' ? undefined : o.mode, ev: o.ev ? o.ev.toFixed(2) : undefined, wb: o.wb === 'asshot' ? undefined : o.wb, tx: o.tx, ty: o.ty }),
  /** A zoom tile of the non-RAW view: pyramid level `l` (0 = full size), column, row. */
  ptile: (id: number, v: string, l: number, tx: number, ty: number) => apiUrl(`photo/${id}/ptile`, { v, l, tx, ty }),
  /** A tile's sensor samples, for demosaicing on this device's GPU. */
  cfatile: (id: number, v: string, wb: WbMode, tx: number, ty: number) => apiUrl(`photo/${id}/cfatile`, { v, wb: wb === 'asshot' ? undefined : wb, tx, ty }),
  /** The tone curve of a RAW view (65536 bytes), for the GPU path. */
  lut: (mode: ToneMode, ev: number, ver: string) => apiUrl('lut', { mode: mode === 'normal' ? undefined : mode, ev: ev ? ev.toFixed(2) : undefined, v: ver || undefined }),
  events: () => apiUrl('events'),
  /** A face or animal, cropped square (JPEG); `big`: twice the size, from a larger image
   * (close-ups for culling). */
  crop: (d: { id: number; v: string }, big = false) => apiUrl(`detection/${d.id}/crop`, { v: d.v, s: big ? 2 : undefined }),
};
