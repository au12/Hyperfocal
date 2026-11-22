// Albums: hand-picked photos in an order of their own. The index keeps them; the
// photos never move. The sidebar lists them, B adds the photos acted on to the target
// album (the last one used), Shift+B picks an album, and in an album shown in its own
// order photos are dragged into place. Every change can be undone.

import { api, type Album, type CollectionSpec } from './api';
import { store } from './store.svelte';

const TARGET_KEY = 'hf.album';

/** The collection id of an album view (`#c=a12`). */
export const albumKey = (id: number) => `a${id}`;
/** What an album view shows: the album, in its own order, across the library. */
export const albumSpec = (id: number): CollectionSpec => ({ folder: '', rec: true, filters: { album: id, sort: 'album', desc: false } });

const plural = (n: number) => (n === 1 ? '1 photo' : `${n.toLocaleString()} photos`);

function loadTarget(): number | null {
  try {
    const v = Number(localStorage.getItem(TARGET_KEY));
    return v > 0 ? v : null;
  } catch {
    return null;
  }
}

class Albums {
  list = $state.raw<Album[]>([]);
  /** Where B puts photos: the album last added to or picked. */
  target = $state<number | null>(loadTarget());
  /** Photos waiting for an album to be picked (the "Add to album" dialog is open). */
  picking = $state.raw<number[] | null>(null);
  loaded = $state(false);
  private loadSeq = 0;

  load = async () => {
    const seq = ++this.loadSeq;
    try {
      const list = await api.albums();
      if (seq !== this.loadSeq) return;
      this.list = list;
      this.loaded = true;
    } catch {
      return; // offline or signed out: keep what is shown
    }
    if (this.target != null && !this.list.some((a) => a.id === this.target)) this.setTarget(null);
    // The album on screen keeps its name in the header in step.
    const cur = this.current;
    const c = store.collection;
    if (cur && c && c.name !== cur.name) store.collection = { ...c, name: cur.name };
  };

  /** Albums changed (here, in another tab or on another device). */
  onEvent = () => {
    this.load();
    if (this.currentId != null) store.scheduleReload();
  };

  byId = (id: number | null | undefined): Album | undefined => (id == null ? undefined : this.list.find((a) => a.id === id));

  /** The id of the album shown, if one is. */
  get currentId(): number | null {
    const id = store.collection?.id;
    return typeof id === 'string' && /^a\d+$/.test(id) ? +id.slice(1) : null;
  }

  get current(): Album | undefined {
    return this.byId(this.currentId);
  }

  /** The album shown in its own order: photos can be dragged into place. */
  get arranging(): boolean {
    return this.currentId != null && store.filters.sort === 'album' && !store.filters.desc && store.view === 'grid';
  }

  open = (a: Album) => {
    return store.applyCollection({ id: albumKey(a.id), name: a.name, spec: albumSpec(a.id) });
  };

  setTarget = (id: number | null) => {
    this.target = id;
    try {
      if (id == null) localStorage.removeItem(TARGET_KEY);
      else localStorage.setItem(TARGET_KEY, String(id));
    } catch {
      /* private mode */
    }
  };

  private undoAction(batch: string | null | undefined) {
    return batch ? { label: 'Undo', run: () => store.undo(batch) } : undefined;
  }

  create = async (name: string, ids: number[] = []): Promise<Album | undefined> => {
    try {
      const a = await api.createAlbum(name, ids);
      this.setTarget(a.id);
      await this.load();
      store.toast(a.added.length ? `Created “${a.name}” with ${plural(a.added.length)}` : `Created “${a.name}”`, 'ok', this.undoAction(a.batch));
      if (store.detail && ids.includes(store.detail.id)) store.loadDetail();
      return this.byId(a.id) ?? a;
    } catch (e) {
      store.toast(`Not created: ${(e as Error).message}`, 'error');
    }
  };

  add = async (albumId: number, ids: number[]) => {
    if (!ids.length) return;
    const name = this.byId(albumId)?.name ?? 'the album';
    try {
      const r = await api.albumAdd(albumId, ids);
      this.setTarget(albumId);
      if (r.added.length) store.toast(`Added ${plural(r.added.length)} to “${name}”`, 'ok', this.undoAction(r.batch));
      else store.toast(ids.length === 1 ? `Already in “${name}”` : `Already all in “${name}”`, 'info');
      this.load();
      if (this.currentId === albumId) store.reload(true);
      if (store.detail && ids.includes(store.detail.id)) store.loadDetail();
    } catch (e) {
      store.toast(`Not added: ${(e as Error).message}`, 'error');
    }
  };

  /** B: the photos acted on go to the target album (with none yet, the picker opens). */
  addToTarget = () => {
    const ids = store.targets();
    if (!ids.length) return;
    const t = this.byId(this.target);
    if (t) this.add(t.id, ids);
    else this.pick(ids);
  };

  /** Opens the "Add to album" dialog for these photos (default: the ones acted on). */
  pick = (ids?: number[]) => {
    const t = ids ?? store.targets();
    if (!t.length) {
      store.toast('Select photos first', 'info');
      return;
    }
    this.picking = t;
  };

  /** Takes photos out of the album shown. Their files stay where they are. */
  remove = async (idsIn?: number[]) => {
    const album = this.current;
    if (!album) return;
    const ids = idsIn ?? store.targets();
    if (!ids.length) return;
    store.dropFromList(ids);
    try {
      const r = await api.albumRemove(album.id, ids);
      store.toast(`Took ${plural(r.removed.length)} out of “${album.name}”`, 'ok', this.undoAction(r.batch));
      this.load();
    } catch (e) {
      store.toast(`Not taken out: ${(e as Error).message}`, 'error');
      store.reload(true);
    }
  };

  /** Puts photos of the album shown (in their order on screen) before `before`, or at
   * the end: on screen at once, then on the server. */
  move = async (ids: number[], before: number | null) => {
    const album = this.current;
    if (!album || !ids.length) return;
    const set = new Set(ids);
    const order = store.list.id.filter((id) => !set.has(id));
    const moving = store.list.id.filter((id) => set.has(id));
    const at = before == null ? order.length : order.indexOf(before);
    if (at < 0 || !moving.length) return;
    order.splice(at, 0, ...moving);
    if (order.every((id, i) => id === store.list.id[i])) return;
    store.reorderList(order);
    try {
      const r = await api.albumOrder(album.id, moving, before);
      if (r.batch) store.toast(`Moved ${plural(moving.length)} in “${album.name}”`, 'ok', this.undoAction(r.batch), 3500);
      this.load();
    } catch (e) {
      store.toast(`Not moved: ${(e as Error).message}`, 'error');
      store.reload(true);
    }
  };

  rename = async (a: Album, name: string) => {
    const n = name.trim();
    if (!n || n === a.name) return;
    try {
      const r = await api.editAlbum(a.id, { name: n });
      store.toast(`Renamed to “${n}”`, 'ok', this.undoAction(r.batch));
      await this.load();
    } catch (e) {
      store.toast(`Not renamed: ${(e as Error).message}`, 'error');
    }
  };

  setNote = async (a: Album, note: string) => {
    try {
      await api.editAlbum(a.id, { note: note.trim() || null });
      await this.load();
    } catch (e) {
      store.toast(`Not saved: ${(e as Error).message}`, 'error');
    }
  };

  /** Makes a photo of the album shown its cover. */
  setCover = async (photo: number) => {
    const a = this.current;
    if (!a) return;
    try {
      const r = await api.editAlbum(a.id, { cover: photo });
      store.toast(`New cover for “${a.name}”`, 'ok', this.undoAction(r.batch));
      await this.load();
    } catch (e) {
      store.toast(`Not set: ${(e as Error).message}`, 'error');
    }
  };

  /** Deletes an album. Its photos stay where they are. */
  deleteAlbum = async (a: Album) => {
    try {
      if (this.currentId === a.id) await store.openLibrary();
      const r = await api.deleteAlbum(a.id);
      store.toast(`Deleted the album “${a.name}”. Its photos stay where they are.`, 'ok', this.undoAction(r.batch), 8000);
      await this.load();
    } catch (e) {
      store.toast(`Not deleted: ${(e as Error).message}`, 'error');
    }
  };

  forgetMissing = async (a: Album) => {
    try {
      const r = await api.albumForget(a.id);
      store.toast(`Forgot ${plural(r.forgotten)} no longer in the library`, 'ok', this.undoAction(r.batch));
      await this.load();
    } catch (e) {
      store.toast(`Not done: ${(e as Error).message}`, 'error');
    }
  };

  /** Sends a whole album to Immich (into an album of the same name, by default). */
  sendToImmich = async (a: Album) => {
    try {
      const root = store.roots[0]?.id;
      const list = await api.photos({ folder: root, rec: 1, album: a.id, sort: 'album' });
      if (!list.id.length) {
        store.toast(`“${a.name}” has no photos to send`, 'info');
        return;
      }
      store.openImmich(list.id, a.name);
    } catch (e) {
      store.toast(`Not sent: ${(e as Error).message}`, 'error');
    }
  };

  /** New order of the albums in the sidebar. */
  arrange = async (ids: number[]) => {
    const byId = new Map(this.list.map((a) => [a.id, a]));
    this.list = ids.map((id) => byId.get(id)).filter((a): a is Album => !!a);
    try {
      await api.orderAlbums(ids);
    } catch (e) {
      store.toast(`Not saved: ${(e as Error).message}`, 'error');
      this.load();
    }
  };
}

export const albums = new Albums();
