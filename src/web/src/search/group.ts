// match[] → per-file groups (SPEC-006 §5.9, D37).
//
// Pure and separate from the store because the ordering rule is the whole point
// and it is easy to get wrong: results stream in from a **parallel** walk, so
// two matches in the same file can be separated by matches from three other
// files. Grouping must therefore be incremental — append to an existing group
// rather than starting a new one — and the group order must be the order each
// file *first* appeared, not alphabetical and not the order of the last arrival.

import type { SearchMatch } from '../api/search';

export interface FileGroup {
  path: string;
  matches: SearchMatch[];
}

/**
 * Group matches by file, preserving first-appearance order of files and arrival
 * order within each file.
 *
 * A `Map` carries insertion order by spec, which is exactly the guarantee this
 * needs — sorting afterwards would throw it away.
 */
export function groupByFile(matches: readonly SearchMatch[]): FileGroup[] {
  const groups = new Map<string, FileGroup>();
  for (const match of matches) {
    const existing = groups.get(match.path);
    if (existing) {
      existing.matches.push(match);
    } else {
      groups.set(match.path, { path: match.path, matches: [match] });
    }
  }
  return [...groups.values()];
}

/**
 * Append one streamed match into an existing group list, in place.
 *
 * The store calls this per event rather than re-running `groupByFile` over the
 * whole array: a 2000-result search would otherwise be 2000 regroupings, and the
 * array identity churn would re-render every row each time.
 *
 * Returns the list it was given, so callers can write `groups = pushMatch(...)`
 * when they need Vue to see a change.
 */
export function pushMatch(groups: FileGroup[], match: SearchMatch): FileGroup[] {
  const last = groups[groups.length - 1];
  // Fast path: streamed matches from one file usually arrive consecutively.
  if (last && last.path === match.path) {
    last.matches.push(match);
    return groups;
  }
  const existing = groups.find((g) => g.path === match.path);
  if (existing) {
    existing.matches.push(match);
  } else {
    groups.push({ path: match.path, matches: [match] });
  }
  return groups;
}

/** Total matches across all groups — the count the header shows while streaming. */
export function countMatches(groups: readonly FileGroup[]): number {
  return groups.reduce((sum, g) => sum + g.matches.length, 0);
}

/** Last `/`-separated segment, for the group header. */
export function fileName(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut === -1 ? path : path.slice(cut + 1);
}

/** Everything before the last `/`, empty at the project root. */
export function dirName(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut === -1 ? '' : path.slice(0, cut);
}
