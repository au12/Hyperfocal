// Justified ("rows of equal height") layout, Augenblick/Flickr style.

export interface LRow {
  top: number;
  height: number;
  start: number;
  end: number; // exclusive
  x: Float32Array; // left of each item
  w: Float32Array; // width of each item
}

export interface Layout {
  rows: LRow[];
  height: number;
  rowOf: Int32Array; // item index -> row index
}

export function aspectOf(w: number, h: number, o: number): number {
  let a = w > 0 && h > 0 ? w / h : 1.5;
  if (o >= 5 && o <= 8) a = 1 / a;
  return Math.min(5, Math.max(0.25, a));
}

export function justify(aspects: ArrayLike<number>, width: number, target: number, gap: number): Layout {
  const n = aspects.length;
  const rows: LRow[] = [];
  const rowOf = new Int32Array(n);
  if (width <= 0 || n === 0) return { rows, height: 0, rowOf };
  let top = 0;
  let start = 0;
  while (start < n) {
    let sum = 0;
    let end = start;
    while (end < n) {
      sum += aspects[end];
      end++;
      if (sum * target + (end - start - 1) * gap >= width) break;
    }
    const count = end - start;
    const avail = width - (count - 1) * gap;
    const full = sum * target + (count - 1) * gap >= width;
    let h = full ? avail / sum : target;
    h = Math.min(h, target * 1.8);
    const x = new Float32Array(count);
    const w = new Float32Array(count);
    let cx = 0;
    for (let k = 0; k < count; k++) {
      const iw = full && k === count - 1 ? width - cx : aspects[start + k] * h;
      x[k] = cx;
      w[k] = Math.max(1, iw);
      cx += iw + gap;
      rowOf[start + k] = rows.length;
    }
    rows.push({ top, height: h, start, end, x, w });
    top += h + gap;
    start = end;
  }
  return { rows, height: Math.max(0, top - gap), rowOf };
}

/** First row whose bottom is below `y`. */
export function rowAt(rows: LRow[], y: number): number {
  let lo = 0;
  let hi = rows.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (rows[mid].top + rows[mid].height < y) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
