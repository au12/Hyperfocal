// Imperative hooks that components register so global shortcuts can reach them.
export const viewerCtl = {
  zoomToggle: () => {},
  fit: () => {},
  zoomIn: () => {},
  zoomOut: () => {},
};
/** The compare view's zoom commands (the active pane, or all when synced). */
export const compareCtl = {
  zoomToggle: () => {},
  fit: () => {},
  zoomIn: () => {},
  zoomOut: () => {},
};
export const gridCtl = {
  vertical: (_d: number) => {},
  /** The photo a row above (-1) or below (1) a grid index, nearest by centre, or null. */
  rowNeighbor: (_i: number, _d: number): number | null => null,
  page: (_d: number) => {},
  /** Scrolls a page down (1) or up (-1) and returns the photo a page from the cursor. */
  pageTarget: (_d: number): number | null => null,
  reveal: (_i: number) => {},
};
export const uiCtl = {
  focusSearch: () => {},
  editTitle: () => {},
  addKeyword: () => {},
};

export function toggleFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
  else document.documentElement.requestFullscreen?.().catch(() => {});
}
