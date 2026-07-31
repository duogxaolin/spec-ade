// number[] → SVG path, plus the byte/duration formatters the panel needs
// (SPEC-006 §5.9, D39–D40).
//
// No chart library ([SPEC-006 §5.9]): 60 points is one `<path d="…">`, and the
// smallest charting dependency is ~40 KB to draw a line.
//
// The interesting cases are all degenerate: an empty history (the first 3s
// before any sample lands), a single point (after the first sample), and a flat
// series (an idle machine reporting 0.0% forever). Each one divides by something
// that is zero, and `NaN` in a `d` attribute makes the whole path disappear.

/** Points kept per series (§5.9): 60 × 3s ≈ 3 minutes of history. */
export const HISTORY_LIMIT = 60;

export interface SparklineOptions {
  width?: number;
  height?: number;
  /** Fix the top of the scale, e.g. 100 for a percentage. Otherwise the max value. */
  max?: number;
  /** Fix the bottom of the scale. Defaults to 0, not the min — a CPU chart that
   *  rebases on its own minimum makes 41%→42% look like a spike. */
  min?: number;
}

/**
 * Build the `d` attribute for a polyline through `values`.
 *
 * Returns `''` for an empty series: an empty `d` renders nothing, whereas a
 * malformed one is a console error on every frame.
 */
export function sparklinePath(
  values: readonly number[],
  options: SparklineOptions = {},
): string {
  const width = options.width ?? 100;
  const height = options.height ?? 24;
  if (values.length === 0) return '';

  const min = options.min ?? 0;
  const rawMax = options.max ?? Math.max(...values, min);
  // A flat series (max === min) would divide by zero. Pin it to the baseline —
  // a flat line at the bottom is the honest picture of "nothing is happening".
  const span = rawMax - min;
  const scaleY = (value: number): number => {
    if (span <= 0) return height;
    const clamped = Math.min(Math.max(value, min), rawMax);
    return height - ((clamped - min) / span) * height;
  };

  // One point has no gaps to divide by, so it draws a flat segment across the
  // full width rather than a zero-length path nobody can see.
  if (values.length === 1) {
    const y = round(scaleY(values[0]));
    return `M 0 ${y} L ${round(width)} ${y}`;
  }

  const step = width / (values.length - 1);
  return values
    .map((value, i) => `${i === 0 ? 'M' : 'L'} ${round(i * step)} ${round(scaleY(value))}`)
    .join(' ');
}

/**
 * The same shape closed along the bottom, for a filled area under the line.
 *
 * Empty in the same case as `sparklinePath`, and for the same reason.
 */
export function sparklineArea(
  values: readonly number[],
  options: SparklineOptions = {},
): string {
  const line = sparklinePath(values, options);
  if (line === '') return '';
  const width = options.width ?? 100;
  const height = options.height ?? 24;
  return `${line} L ${round(width)} ${round(height)} L 0 ${round(height)} Z`;
}

/** Two decimals, without the `1.00` noise — keeps the `d` attribute readable. */
function round(n: number): number {
  return Number.isFinite(n) ? Math.round(n * 100) / 100 : 0;
}

/**
 * Append a point, dropping the oldest past `limit`.
 *
 * Returns a new array so Vue's reactivity sees the change; the histories are
 * ≤60 numbers, so copying is cheaper than the tracking a mutable ring buffer
 * would need.
 */
export function pushPoint(
  history: readonly number[],
  value: number,
  limit = HISTORY_LIMIT,
): number[] {
  const next = [...history, value];
  return next.length > limit ? next.slice(next.length - limit) : next;
}

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];

/**
 * Bytes → human string, binary units (1024).
 *
 * Binary because every number it formats comes from `sysinfo`, which reports
 * what the kernel reports: `total` on a 16 GiB machine is 17 179 869 184, and
 * dividing by 1000 would render that as "17.18 GB".
 */
export function formatBytes(bytes: number, decimals = 1): string {
  if (!Number.isFinite(bytes)) return '—';
  const negative = bytes < 0;
  let value = Math.abs(bytes);
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Whole bytes have no fractional part worth showing: "512 B", not "512.0 B".
  const text = unit === 0 ? String(Math.round(value)) : value.toFixed(decimals);
  return `${negative ? '-' : ''}${text} ${BYTE_UNITS[unit]}`;
}

/** Seconds → `3d 4h`, `4h 12m`, `12m 5s`, `5s` — two units, largest first. */
export function formatUptime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  const total = Math.floor(seconds);
  const days = Math.floor(total / 86400);
  const hours = Math.floor((total % 86400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;

  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${secs}s`;
  return `${secs}s`;
}

/** 0–100 → `"41.2%"`. Clamped, because a multithreaded process can report 780. */
export function formatPercent(value: number, decimals = 1): string {
  if (!Number.isFinite(value)) return '—';
  return `${value.toFixed(decimals)}%`;
}
