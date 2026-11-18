// Keyboard shortcuts. The default preset mirrors FastRawViewer's default bindings;
// plain 0-5 / 6-9 work as in Lightroom and Excire. A Lightroom preset swaps in P/X/U
// for pick/reject/unflag.

export type Action =
  | 'next' | 'prev' | 'first' | 'last' | 'next2' | 'prev2' | 'next3' | 'prev3' | 'pageDown' | 'pageUp' | 'up' | 'down'
  | 'open' | 'escape' | 'toggleGrid' | 'zoomToggle' | 'fit' | 'zoomIn' | 'zoomOut' | 'fullscreen'
  | 'source' | 'peaking' | 'sharpen' | 'shadows' | 'highlights' | 'linear' | 'invert' | 'over' | 'under' | 'wb'
  | 'evToggle' | 'evUp' | 'evDown' | 'evUpBig' | 'evDownBig' | 'evReset'
  | 'rate0' | 'rate1' | 'rate2' | 'rate3' | 'rate4' | 'rate5' | 'rateReject' | 'rateUp' | 'rateDown'
  | 'labelRed' | 'labelYellow' | 'labelGreen' | 'labelBlue' | 'labelPurple'
  | 'pick' | 'unflag' | 'reject' | 'editTitle' | 'keywords' | 'location'
  | 'selectToggle' | 'selectNext' | 'selectPrev' | 'selectAll' | 'selectNone' | 'selectInvert'
  | 'rotateCcw' | 'rotateCw' | 'rotate180'
  | 'histogram' | 'info' | 'filmstrip' | 'folders' | 'hidePanels' | 'help' | 'search' | 'undo' | 'map';

type Bindings = Partial<Record<Action, string[]>>;

const FRV: Bindings = {
  next: ['ArrowRight', 'Space', 'Ctrl+ArrowRight'],
  prev: ['ArrowLeft', 'Backspace', 'Ctrl+ArrowLeft'],
  first: ['Home', 'Ctrl+Shift+ArrowLeft'],
  last: ['End', 'Ctrl+Shift+ArrowRight'],
  next2: ['Ctrl+Alt+ArrowRight'],
  prev2: ['Ctrl+Alt+ArrowLeft'],
  next3: ['Ctrl+Alt+Shift+ArrowRight'],
  prev3: ['Ctrl+Alt+Shift+ArrowLeft'],
  pageDown: ['PageDown'],
  pageUp: ['PageUp'],
  up: ['ArrowUp'],
  down: ['ArrowDown'],
  open: ['Enter'],
  escape: ['Escape'],
  toggleGrid: ['G'],
  zoomToggle: ['Z'],
  fit: ['Ctrl+0'],
  zoomIn: ['Ctrl+Equal', 'Ctrl+Shift+Equal', 'Ctrl+NumpadAdd'],
  zoomOut: ['Ctrl+Minus', 'Ctrl+NumpadSubtract'],
  fullscreen: ['F'],
  source: ['J'],
  peaking: ['P'],
  sharpen: ['S'],
  shadows: ['Shift+S'],
  highlights: ['Shift+H'],
  linear: ['Shift+L'],
  // FastRawViewer has no default key for "Invert image colors".
  invert: ['Shift+I'],
  over: ['O'],
  under: ['U'],
  wb: ['W'],
  evToggle: ['A'],
  evReset: ['Shift+A'],
  evUp: ['Alt+Equal'],
  evDown: ['Alt+Minus'],
  evUpBig: ['Ctrl+Alt+Equal'],
  evDownBig: ['Ctrl+Alt+Minus'],
  rate0: ['0', 'Alt+0'],
  rate1: ['1', 'Alt+1'],
  rate2: ['2', 'Alt+2'],
  rate3: ['3', 'Alt+3'],
  rate4: ['4', 'Alt+4'],
  rate5: ['5', 'Alt+5'],
  rateReject: ['Alt+Delete'],
  rateDown: ['Alt+Comma'],
  rateUp: ['Alt+Period'],
  labelRed: ['6', 'Alt+6'],
  labelYellow: ['7', 'Alt+7'],
  labelGreen: ['8', 'Alt+8'],
  labelBlue: ['9', 'Alt+9'],
  labelPurple: ['Alt+Shift+9'],
  pick: ['Alt+P', 'Backquote'],
  unflag: ['Alt+U'],
  reject: ['Ctrl+Shift+Delete', 'Delete'],
  editTitle: ['Alt+D'],
  keywords: ['K'],
  location: ['L'],
  selectToggle: ['Ctrl+Slash'],
  selectNext: ['Shift+ArrowRight', 'Ctrl+Period'],
  selectPrev: ['Shift+ArrowLeft', 'Ctrl+Comma'],
  selectAll: ['Ctrl+A'],
  selectNone: ['Ctrl+D'],
  selectInvert: ['Ctrl+I'],
  rotateCcw: ['Ctrl+BracketLeft'],
  rotateCw: ['Ctrl+BracketRight'],
  rotate180: ['Ctrl+Shift+BracketRight'],
  histogram: ['F2', 'H'],
  info: ['F3', 'F5', 'I'],
  filmstrip: ['F6'],
  folders: ['F7'],
  hidePanels: ['Tab'],
  help: ['F1', 'Shift+Slash'],
  search: ['Ctrl+F', 'Slash'],
  undo: ['Ctrl+Z'],
  map: ['M'],
};

const LR: Bindings = {
  ...FRV,
  pick: ['P'],
  unflag: ['U'],
  reject: ['X', 'Delete', 'Ctrl+Shift+Delete'],
  peaking: ['Shift+P'],
  under: ['Shift+U'],
};

export const PRESETS = { frv: FRV, lr: LR };

export const ACTION_LABELS: [string, [Action, string][]][] = [
  ['Navigation', [
    ['next', 'Next photo'], ['prev', 'Previous photo'], ['first', 'First photo'], ['last', 'Last photo'],
    ['next2', 'Jump +2'], ['next3', 'Jump +3'], ['toggleGrid', 'Grid / single view'], ['open', 'Open photo'],
    ['escape', 'Back / exit fullscreen'], ['fullscreen', 'Full screen'],
  ]],
  ['View', [
    ['source', 'Switch RAW / embedded JPEG'], ['zoomToggle', 'Fit / 100%'], ['fit', 'Fit to window'], ['zoomIn', 'Zoom in'], ['zoomOut', 'Zoom out'],
    ['sharpen', 'Screen sharpening (off / 1 / 2)'], ['peaking', 'Focus peaking (edges / details / both)'],
    ['over', 'Show overexposed areas'], ['under', 'Show underexposed areas'],
    ['shadows', 'Shadow boost'], ['highlights', 'Highlight inspection'], ['linear', 'Linear mode'], ['invert', 'Invert colours'],
    ['wb', 'White balance (as shot / daylight / auto)'], ['evToggle', 'Exposure correction on/off'],
    ['evUp', 'Exposure +⅓ EV'], ['evDown', 'Exposure −⅓ EV'], ['evUpBig', 'Exposure +1 EV'], ['evDownBig', 'Exposure −1 EV'], ['evReset', 'Reset exposure'],
    ['histogram', 'Histogram'], ['info', 'Info & metadata panel'], ['filmstrip', 'Filmstrip'], ['folders', 'Folders panel'], ['hidePanels', 'Hide all panels'],
  ]],
  ['Culling & metadata', [
    ['rate1', 'Rating 1'], ['rate5', 'Rating 5'], ['rate0', 'Clear rating'], ['rateUp', 'Rating +1'], ['rateDown', 'Rating −1'],
    ['labelRed', 'Red label'], ['labelYellow', 'Yellow label'], ['labelGreen', 'Green label'], ['labelBlue', 'Blue label'], ['labelPurple', 'Purple label'],
    ['pick', 'Pick flag'], ['unflag', 'Clear flag'], ['reject', 'Move to reject folder (restore when rejected)'], ['rateReject', 'Reject rating (−1)'],
    ['editTitle', 'Edit title & caption'], ['keywords', 'Add keywords'], ['location', 'Set location on map'],
    ['rotateCcw', 'Rotate left'], ['rotateCw', 'Rotate right'], ['undo', 'Undo last change'],
  ]],
  ['Selection & search', [
    ['selectToggle', 'Select / deselect'], ['selectNext', 'Select and next'], ['selectAll', 'Select all'], ['selectNone', 'Select none'], ['selectInvert', 'Invert selection'],
    ['search', 'Search'], ['map', 'Map view'], ['help', 'Keyboard shortcuts'],
  ]],
];

const CODE_NAMES: Record<string, string> = {
  Space: 'Space', Backspace: 'Backspace', Delete: 'Delete', Enter: 'Enter', NumpadEnter: 'Enter', Escape: 'Escape', Tab: 'Tab',
  ArrowLeft: 'ArrowLeft', ArrowRight: 'ArrowRight', ArrowUp: 'ArrowUp', ArrowDown: 'ArrowDown', Home: 'Home', End: 'End',
  PageUp: 'PageUp', PageDown: 'PageDown', Equal: 'Equal', Minus: 'Minus', Comma: 'Comma', Period: 'Period', Slash: 'Slash',
  BracketLeft: 'BracketLeft', BracketRight: 'BracketRight', Backquote: 'Backquote', NumpadAdd: 'NumpadAdd', NumpadSubtract: 'NumpadSubtract',
};

/** Normalised key string for an event, e.g. "Ctrl+Shift+Delete", "Alt+5", "J". */
export function keyOf(e: KeyboardEvent): string {
  let k: string | undefined;
  const c = e.code;
  if (c.startsWith('Key')) k = c.slice(3);
  else if (c.startsWith('Digit')) k = c.slice(5);
  else if (c.startsWith('Numpad') && /\d$/.test(c)) k = c.slice(6);
  else if (/^F\d{1,2}$/.test(c)) k = c;
  else k = CODE_NAMES[c];
  if (!k) return '';
  const mods = [];
  if (e.ctrlKey || e.metaKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  return [...mods, k].join('+');
}

export function actionFor(e: KeyboardEvent, preset: 'frv' | 'lr'): Action | null {
  const key = keyOf(e);
  if (!key) return null;
  const b = PRESETS[preset];
  for (const [a, keys] of Object.entries(b) as [Action, string[]][]) {
    if (keys.includes(key)) return a;
  }
  return null;
}

export function keysFor(a: Action, preset: 'frv' | 'lr'): string[] {
  return PRESETS[preset][a] ?? [];
}

export function prettyKey(k: string): string {
  const mac = navigator.platform.toLowerCase().includes('mac');
  return k
    .replace('Ctrl', mac ? '⌘' : 'Ctrl')
    .replace('Alt', mac ? '⌥' : 'Alt')
    .replace('Shift', '⇧')
    .replace('ArrowLeft', '←').replace('ArrowRight', '→').replace('ArrowUp', '↑').replace('ArrowDown', '↓')
    .replace('Equal', '=').replace('Minus', '−').replace('Comma', ',').replace('Period', '.').replace('Slash', '/')
    .replace('BracketLeft', '[').replace('BracketRight', ']').replace('Backquote', '`').replace('Delete', 'Del')
    .replace('NumpadAdd', 'Num+').replace('NumpadSubtract', 'Num−');
}
