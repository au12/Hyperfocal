import type { MlStatus } from './api';

const size = (b: number) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.round(b / 1e6)} MB`);

/** Downloaded share of the AI models (0..1), while they download. */
export function mlProgress(m: MlStatus | null | undefined): number | null {
  return m && m.phase === 'downloading' && m.total > 0 ? Math.min(1, m.done / m.total) : null;
}

/** Where the AI engine is, in a few words: "Downloading the AI models · 412 MB of 1.5 GB". */
export function mlState(m: MlStatus | null | undefined): string {
  if (!m || !m.enabled) return 'The AI features are turned off';
  switch (m.phase) {
    case 'downloading':
      return m.total > 0 ? `Downloading the AI models · ${size(m.done)} of ${size(m.total)}` : 'Downloading the AI models';
    case 'starting':
      return 'Starting the AI engine';
    case 'ready':
      return m.device === 'cuda' ? `AI engine running on the GPU${m.gpu ? ` (${m.gpu})` : ''}` : 'AI engine running on the CPU';
    case 'asleep':
      return `AI engine asleep (${m.device === 'cuda' ? 'GPU' : 'memory'} freed), starts when needed`;
    default:
      return m.error ?? 'The AI engine is not running';
  }
}

/** The AI engine is on its way (models downloading, or loading). */
export const mlPreparing = (m: MlStatus | null | undefined) => !!m && m.enabled && (m.phase === 'downloading' || m.phase === 'starting');

export { size as mlSize };
