// Advanced settings, shared by the whole library (Settings → Advanced). The app reads a
// few of them itself (the sharpness words); the server uses all of them.

import { api, type Tuning } from './api';

class TuningStore {
  values = $state.raw<Tuning | null>(null);
  defaults = $state.raw<Tuning | null>(null);

  load = async () => {
    try {
      const t = await api.tuning();
      this.values = t.values;
      this.defaults = t.defaults;
    } catch {
      /* the defaults below stand in */
    }
  };

  set = (t: { values: Tuning; defaults: Tuning }) => {
    this.values = t.values;
    this.defaults = t.defaults;
  };

  /** Subjects less sharp than this are blurry. */
  blurry = () => this.values?.cull_blurry ?? 0.3;
  /** Subjects at least this sharp are sharp. */
  sharp = () => this.values?.cull_sharp ?? 0.7;
}

export const tuning = new TuningStore();
