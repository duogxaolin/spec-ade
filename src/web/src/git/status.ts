// Turning a flat `entries[]` into the four groups the panel renders (C41, C42).
//
// Pure on purpose, like `chat/*.ts` in SPEC-004: the grouping rules are the part
// worth testing, and testing them through a mounted component would only test
// vue-test-utils.
//
// The rule that shapes everything here: `index` and `worktree` are independent
// axes. A file staged and then edited again is `MM` in `git status`, and it belongs
// in **both** Staged and Changed — because staging it again and discarding the
// worktree edit are two different actions on two different versions of the file.
// Collapsing it into one row would make one of those actions unreachable (C42).

import type { ChangeState, StatusEntry } from '../api/git';

/** The four buckets, in the order the panel shows them. */
export type GroupId = 'conflicted' | 'staged' | 'changed' | 'untracked';

/** One row. `key` is unique across groups, which an `MM` file needs (C42). */
export interface GroupedEntry {
  key: string;
  group: GroupId;
  path: string;
  /** Basename, for the row label. */
  name: string;
  /** Directory part without a trailing slash, `''` at the root. */
  dir: string;
  /** Single-letter status marker, git's own vocabulary. */
  glyph: string;
  /** Long form for the `title`/`aria-label`. */
  label: string;
  /** A rename's source path, when this row is a rename. */
  origPath: string | null;
  entry: StatusEntry;
}

export interface StatusGroup {
  id: GroupId;
  /** Header text. Vietnamese, like the rest of the UI. */
  title: string;
  entries: GroupedEntry[];
}

/** Group order: what needs attention first, what is furthest from a commit last. */
const GROUP_ORDER: GroupId[] = ['conflicted', 'staged', 'changed', 'untracked'];

const GROUP_TITLES: Record<GroupId, string> = {
  conflicted: 'Xung đột',
  staged: 'Đã stage',
  changed: 'Thay đổi',
  untracked: 'Chưa theo dõi',
};

/**
 * Glyph per change state — git's letters, so anyone who has read `git status`
 * already knows them.
 *
 * `none` is included for completeness and never actually renders: an entry only
 * reaches a group because the axis that group reads is *not* `none`.
 */
export const STATE_GLYPH: Record<ChangeState, string> = {
  none: '·',
  new: 'A',
  modified: 'M',
  deleted: 'D',
  renamed: 'R',
  typechange: 'T',
};

const STATE_LABEL: Record<ChangeState, string> = {
  none: 'không đổi',
  new: 'mới',
  modified: 'đã sửa',
  deleted: 'đã xoá',
  renamed: 'đã đổi tên',
  typechange: 'đổi loại',
};

/** Which glyph a row shows, given the group it landed in. */
export function glyphFor(entry: StatusEntry, group: GroupId): string {
  if (group === 'conflicted') return 'U';
  // Untracked is `worktree: 'new'`, but git writes it `??`, not `A` — `A` means
  // "added to the index", which is the opposite of untracked.
  if (group === 'untracked') return '?';
  return STATE_GLYPH[group === 'staged' ? entry.index : entry.worktree];
}

function labelFor(entry: StatusEntry, group: GroupId): string {
  if (group === 'conflicted') return 'xung đột';
  if (group === 'untracked') return 'chưa theo dõi';
  return STATE_LABEL[group === 'staged' ? entry.index : entry.worktree];
}

function rowFor(entry: StatusEntry, group: GroupId): GroupedEntry {
  const slash = entry.path.lastIndexOf('/');
  return {
    key: `${group}:${entry.path}`,
    group,
    path: entry.path,
    name: slash === -1 ? entry.path : entry.path.slice(slash + 1),
    dir: slash === -1 ? '' : entry.path.slice(0, slash),
    glyph: glyphFor(entry, group),
    label: labelFor(entry, group),
    origPath: entry.origPath,
    entry,
  };
}

/**
 * Split `entries` into the four groups.
 *
 * A conflicted entry goes **only** to `conflicted`: while a file is unmerged its
 * index holds three stages rather than a stageable version, so showing it under
 * Staged would offer an action git will refuse.
 *
 * Empty groups are returned as empty arrays rather than omitted — the caller
 * decides not to render them (C46), which keeps this function's answer complete.
 */
export function groupEntries(entries: StatusEntry[]): StatusGroup[] {
  const buckets: Record<GroupId, GroupedEntry[]> = {
    conflicted: [],
    staged: [],
    changed: [],
    untracked: [],
  };

  for (const entry of entries) {
    if (entry.conflicted) {
      buckets.conflicted.push(rowFor(entry, 'conflicted'));
      continue;
    }
    if (entry.index !== 'none') {
      buckets.staged.push(rowFor(entry, 'staged'));
    }
    if (entry.worktree === 'new') {
      buckets.untracked.push(rowFor(entry, 'untracked'));
    } else if (entry.worktree !== 'none') {
      buckets.changed.push(rowFor(entry, 'changed'));
    }
  }

  // Sort inside each group, not across: the server already sorts by path, but the
  // store also applies optimistic updates and an SSE frame can arrive mid-edit,
  // so relying on the wire order would make row order depend on timing.
  for (const id of GROUP_ORDER) {
    buckets[id].sort(comparePaths);
  }

  return GROUP_ORDER.map((id) => ({
    id,
    title: GROUP_TITLES[id],
    entries: buckets[id],
  }));
}

/**
 * Path order: directory-aware and case-insensitive, with a stable tiebreak.
 *
 * A plain `a.path < b.path` puts `src/a.ts` next to `src-gen/b.ts` because `-`
 * sorts before `/`, which scatters a directory's files. Comparing segment by
 * segment keeps each directory's rows together.
 */
export function comparePaths(a: { path: string }, b: { path: string }): number {
  const left = a.path.split('/');
  const right = b.path.split('/');

  for (let i = 0; i < Math.min(left.length, right.length); i += 1) {
    const lastLeft = i === left.length - 1;
    const lastRight = i === right.length - 1;
    // A file and a directory at the same position: the directory goes second, so
    // a folder's contents do not interleave with its siblings.
    if (lastLeft !== lastRight) return lastLeft ? -1 : 1;

    const cmp = left[i].localeCompare(right[i], undefined, { sensitivity: 'base' });
    if (cmp !== 0) return cmp;
    // Same segment ignoring case — fall through to the next segment.
  }

  if (left.length !== right.length) return left.length - right.length;
  // Identical ignoring case (`README` vs `readme`, both real on a case-sensitive
  // filesystem): a byte compare keeps the order deterministic.
  return a.path < b.path ? -1 : a.path > b.path ? 1 : 0;
}

/** Groups with at least one row — what the panel actually renders (C46). */
export function nonEmptyGroups(groups: StatusGroup[]): StatusGroup[] {
  return groups.filter((group) => group.entries.length > 0);
}

/**
 * Whether the working tree has anything a commit could include.
 *
 * Conflicts count as "not ready": committing mid-conflict is possible in git but
 * records the markers, so the panel treats resolving as the next step instead.
 */
export function hasStagedWork(entries: StatusEntry[]): boolean {
  return entries.some((entry) => !entry.conflicted && entry.index !== 'none');
}

/** Short summary for the branch bar: `3 staged · 1 thay đổi · 2 mới`. */
export function summarize(entries: StatusEntry[]): string {
  const groups = groupEntries(entries);
  const parts: string[] = [];
  for (const group of groups) {
    if (group.entries.length > 0) parts.push(`${group.entries.length} ${group.title.toLowerCase()}`);
  }
  return parts.join(' · ');
}
