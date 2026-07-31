// text + ranges → renderable segments (SPEC-006 §5.9, D38).
//
// Two traps make this worth its own pure module:
//
// 1. **The ranges are byte offsets.** The server produces them from a `&[u8]`
//    line (`MatchEvent.ranges` are `[start, end)` into the UTF-8 encoding), but a
//    JS string is indexed in UTF-16 code units. `"café".slice(0, 4)` and the
//    server's byte 4 are not the same place. So the text is encoded once and
//    sliced in byte space.
// 2. **Ranges can overlap.** A regex with alternation can report `[0,5)` and
//    `[3,8)` on one line; naively emitting one segment per range would duplicate
//    the shared characters. They are merged before slicing.

import type { SearchMatch } from '../api/search';

export interface Segment {
  text: string;
  /** Render inside `<mark>` when true. */
  match: boolean;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Merge overlapping/touching ranges and drop degenerate ones, sorted by start.
 *
 * Touching ranges (`[0,3)` + `[3,6)`) are merged too: two adjacent `<mark>`s
 * render identically to one, and merging keeps the segment list shorter.
 */
export function normalizeRanges(
  ranges: readonly (readonly [number, number])[],
  limit: number,
): Array<[number, number]> {
  const clean: Array<[number, number]> = [];
  for (const [rawStart, rawEnd] of ranges) {
    const start = Math.max(0, Math.min(rawStart, limit));
    const end = Math.max(0, Math.min(rawEnd, limit));
    if (end > start) clean.push([start, end]);
  }
  clean.sort((a, b) => a[0] - b[0] || a[1] - b[1]);

  const merged: Array<[number, number]> = [];
  for (const range of clean) {
    const last = merged[merged.length - 1];
    if (last && range[0] <= last[1]) {
      last[1] = Math.max(last[1], range[1]);
    } else {
      merged.push([range[0], range[1]]);
    }
  }
  return merged;
}

/**
 * Split a line into alternating plain/highlighted segments.
 *
 * Empty `ranges` (a non-UTF-8 line, per §3.1) yields a single plain segment —
 * a missing highlight beats a wrong one.
 */
export function highlight(
  text: string,
  ranges: readonly (readonly [number, number])[],
): Segment[] {
  if (text === '') return [];
  if (ranges.length === 0) return [{ text, match: false }];

  const bytes = encoder.encode(text);
  const merged = normalizeRanges(ranges, bytes.length);
  if (merged.length === 0) return [{ text, match: false }];

  const segments: Segment[] = [];
  let cursor = 0;
  for (const [start, end] of merged) {
    if (start > cursor) {
      segments.push({ text: decoder.decode(bytes.subarray(cursor, start)), match: false });
    }
    segments.push({ text: decoder.decode(bytes.subarray(start, end)), match: true });
    cursor = end;
  }
  if (cursor < bytes.length) {
    segments.push({ text: decoder.decode(bytes.subarray(cursor)), match: false });
  }
  return segments;
}

/** `highlight` applied to a streamed match. */
export function highlightMatch(match: SearchMatch): Segment[] {
  return highlight(match.text, match.ranges);
}

/** Longest leading run of spaces/tabs shared by every line — for trimming indentation. */
export function commonIndent(lines: readonly string[]): number {
  let min = Infinity;
  for (const line of lines) {
    if (line.trim() === '') continue;
    const indent = line.length - line.trimStart().length;
    if (indent < min) min = indent;
  }
  return min === Infinity ? 0 : min;
}
