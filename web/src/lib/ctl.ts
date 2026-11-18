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
  page: (_d: number) => {},
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
