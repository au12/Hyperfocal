import { api, type DupeGroup, type DupeLevel, type DupeMember, type DupeMode, type DupeReport, type DupeScope } from './api';

/** The Duplicates report and its settings, kept while the dialog is closed (to look at a
 * group in the compare view and come back to the same place). */
class Dupes {
  mode = $state<DupeMode>('exact');
  scope = $state<DupeScope>('library');
  level = $state<DupeLevel>('normal');
  /** Series: the longest pause between two frames, seconds. */
  gap = $state(2);
  /** The folder of the folder scopes. */
  folder = $state<number | null>(null);
  report = $state.raw<DupeReport | null>(null);
  /** The groups still to deal with (rejected and dismissed ones leave). */
  groups = $state.raw<DupeGroup[]>([]);
  loading = $state(false);
  error = $state('');
  /** Keepers picked by hand, by group. */
  keep = $state<Record<string, number>>({});
  /** Groups drawn so far (more as the list scrolls). */
  shown = $state(24);
  /** Scroll position of the list, restored when the dialog opens again. */
  scroll = 0;
  private seq = 0;
  private ctl: AbortController | null = null;

  load = async () => {
    this.ctl?.abort();
    const ctl = (this.ctl = new AbortController());
    const seq = ++this.seq;
    this.loading = true;
    try {
      const folder = this.scope === 'library' ? undefined : (this.folder ?? undefined);
      const r = await api.dupes(
        {
          mode: this.mode,
          scope: folder == null ? 'library' : this.scope,
          folder,
          level: this.mode === 'near' || this.mode === 'visual' ? this.level : undefined,
          gap: this.mode === 'series' ? this.gap : undefined,
        },
        ctl.signal,
      );
      if (seq !== this.seq) return;
      this.report = r;
      this.groups = r.groups;
      this.keep = {};
      this.shown = 24;
      this.scroll = 0;
      this.error = '';
    } catch (e) {
      if (ctl.signal.aborted) return;
      this.error = (e as Error).message;
    } finally {
      if (seq === this.seq) this.loading = false;
    }
  };

  /** The photo of a group that stays: picked by hand, else the suggested one (always a
   * member, so "reject the others" never takes them all). */
  keeperOf = (g: DupeGroup): number => {
    const k = this.keep[g.key] ?? g.keeper;
    return g.members.some((m) => m.id === k) ? k : g.members[0].id;
  };

  /** The members of a group other than its keeper. */
  others = (g: DupeGroup): DupeMember[] => {
    const k = this.keeperOf(g);
    return g.members.filter((m) => m.id !== k);
  };

  /** Photos that moved away leave their groups; a group left with one photo goes. */
  drop = (ids: number[]) => {
    const gone = new Set(ids);
    this.groups = this.groups.map((g) => ({ ...g, members: g.members.filter((m) => !gone.has(m.id)) })).filter((g) => g.members.length > 1);
  };

  dropGroup = (key: string) => {
    this.groups = this.groups.filter((g) => g.key !== key);
  };
}

export const dupes = new Dupes();
