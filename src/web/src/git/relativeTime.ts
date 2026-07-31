// "3 phút trước" for commit timestamps (C43).
//
// Pure and injectable-clock on purpose: a function that reads `Date.now()` itself
// can only be tested by mocking global time, and the interesting cases here are
// all *boundaries* — 59s vs 60s, 23h vs 24h — which need an exact `now`.
//
// Git timestamps are Unix **seconds**, not milliseconds. Passing a JS
// `Date.now()` value where seconds are expected reads as ~55,000 years in the
// future, so the unit is in the parameter name and the future branch below is a
// real code path, not a defensive flourish.

/** Thresholds in seconds, largest unit last. */
const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const MONTH = 30 * DAY;
const YEAR = 365 * DAY;

/**
 * Human-readable age of a commit.
 *
 * @param timeSeconds Unix seconds, as git reports them.
 * @param nowMs Current time in ms; injected so tests can pin a boundary.
 *
 * Boundaries are chosen so no elapsed time falls in a gap: under 60s is "vừa
 * xong", 60s is exactly "1 phút trước". A future timestamp — which happens
 * whenever a commit came from a machine with a fast clock, or from a rebase that
 * preserved an author date — reads "vừa xong" rather than "-3 phút trước".
 */
export function relativeTime(timeSeconds: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(timeSeconds)) return '';

  const elapsed = Math.floor(nowMs / 1000) - Math.floor(timeSeconds);

  // Clock skew: a commit cannot have happened later than now from the user's point
  // of view, and a negative duration is worse than a slightly wrong one.
  if (elapsed < MINUTE) return 'vừa xong';

  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)} phút trước`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)} giờ trước`;
  if (elapsed < MONTH) return `${Math.floor(elapsed / DAY)} ngày trước`;
  if (elapsed < YEAR) return `${Math.floor(elapsed / MONTH)} tháng trước`;
  return `${Math.floor(elapsed / YEAR)} năm trước`;
}

/**
 * Absolute timestamp for the `title` attribute.
 *
 * The relative form is what you read at a glance; the exact one is what you need
 * when it matters, so every relative time in the UI carries this as a tooltip.
 */
export function absoluteTime(timeSeconds: number): string {
  if (!Number.isFinite(timeSeconds)) return '';
  const date = new Date(timeSeconds * 1000);
  if (Number.isNaN(date.getTime())) return '';
  // `sv-SE` gives `YYYY-MM-DD HH:mm` without needing a format string, and sorts
  // lexicographically — handy when these end up in a log the user greps.
  return date.toLocaleString('sv-SE', { timeZoneName: undefined }).slice(0, 16);
}
