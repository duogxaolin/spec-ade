// C41, C42, C46 — the grouping rules.
//
// These assert what git itself says, not what feels tidy. The `MM` case (C42) is
// the one that looks like a bug in a screenshot and is not: the file really is in
// two states at once, and each state has its own action.

import { describe, expect, it } from 'vitest';

import type { StatusEntry } from '../api/git';
import {
  comparePaths,
  glyphFor,
  groupEntries,
  hasStagedWork,
  nonEmptyGroups,
  summarize,
} from './status';

/** A status entry with everything unchanged, to be overridden per case. */
function entry(path: string, over: Partial<StatusEntry> = {}): StatusEntry {
  const index = over.index ?? 'none';
  return {
    path,
    origPath: null,
    index,
    worktree: 'none',
    conflicted: false,
    staged: index !== 'none',
    ...over,
    // `staged` mirrors `index`, so recompute it after the spread rather than
    // trusting a caller to keep the two consistent.
    ...(over.index !== undefined ? { staged: over.index !== 'none' } : {}),
  };
}

/** The group with this id, which `groupEntries` always returns. */
function group(groups: ReturnType<typeof groupEntries>, id: string) {
  const found = groups.find((g) => g.id === id);
  if (!found) throw new Error(`no ${id} group`);
  return found;
}

describe('groupEntries', () => {
  it('puts each entry in the group its axis says', () => {
    const groups = groupEntries([
      entry('staged.txt', { index: 'modified' }),
      entry('changed.txt', { worktree: 'modified' }),
      entry('new.txt', { worktree: 'new' }),
      entry('conflict.txt', { conflicted: true, worktree: 'modified' }),
    ]);

    expect(group(groups, 'staged').entries.map((e) => e.path)).toEqual(['staged.txt']);
    expect(group(groups, 'changed').entries.map((e) => e.path)).toEqual(['changed.txt']);
    expect(group(groups, 'untracked').entries.map((e) => e.path)).toEqual(['new.txt']);
    expect(group(groups, 'conflicted').entries.map((e) => e.path)).toEqual(['conflict.txt']);
  });

  it('always returns the four groups in a fixed order', () => {
    // The order is the panel's layout, so it must not depend on which groups
    // happen to be occupied.
    expect(groupEntries([]).map((g) => g.id)).toEqual([
      'conflicted',
      'staged',
      'changed',
      'untracked',
    ]);
  });

  it('shows an MM file in both Staged and Changed', () => {
    // C42. Staging again and discarding the worktree edit act on two different
    // versions of the file; one row would make one of them unreachable.
    const groups = groupEntries([
      entry('a.txt', { index: 'modified', worktree: 'modified' }),
    ]);

    expect(group(groups, 'staged').entries.map((e) => e.path)).toEqual(['a.txt']);
    expect(group(groups, 'changed').entries.map((e) => e.path)).toEqual(['a.txt']);
    // And the two rows must be distinguishable as list keys, or Vue reuses one
    // DOM node for both.
    const keys = [
      group(groups, 'staged').entries[0].key,
      group(groups, 'changed').entries[0].key,
    ];
    expect(new Set(keys).size).toBe(2);
  });

  it('keeps a conflicted file out of the staged and changed groups', () => {
    // An unmerged file has three index stages rather than a stageable version, so
    // offering "unstage" on it would offer something git refuses.
    const groups = groupEntries([
      entry('a.txt', { conflicted: true, index: 'modified', worktree: 'modified' }),
    ]);

    expect(group(groups, 'conflicted').entries).toHaveLength(1);
    expect(group(groups, 'staged').entries).toHaveLength(0);
    expect(group(groups, 'changed').entries).toHaveLength(0);
  });

  it('splits a path into name and dir for the row label', () => {
    const [row] = group(groupEntries([entry('src/deep/a.rs', { worktree: 'modified' })]), 'changed')
      .entries;
    expect(row.name).toBe('a.rs');
    expect(row.dir).toBe('src/deep');

    const [root] = group(groupEntries([entry('a.rs', { worktree: 'modified' })]), 'changed').entries;
    expect(root.name).toBe('a.rs');
    expect(root.dir).toBe('');
  });

  it('carries a rename source through to the row', () => {
    const [row] = group(
      groupEntries([entry('new.rs', { index: 'renamed', origPath: 'old.rs' })]),
      'staged',
    ).entries;
    expect(row.origPath).toBe('old.rs');
  });

  it('sorts inside each group instead of trusting the wire order', () => {
    // The server sorts, but the store applies optimistic updates and an SSE frame
    // can land mid-edit — so row order must not depend on arrival order.
    const groups = groupEntries([
      entry('z.txt', { worktree: 'modified' }),
      entry('a.txt', { worktree: 'modified' }),
      entry('m.txt', { worktree: 'modified' }),
    ]);
    expect(group(groups, 'changed').entries.map((e) => e.path)).toEqual([
      'a.txt',
      'm.txt',
      'z.txt',
    ]);
  });
});

describe('glyphFor', () => {
  it('uses git letters for each state', () => {
    // C41: six states, and the letters are git's own so anyone who has read
    // `git status` already knows them.
    const cases: Array<[StatusEntry['index'], string]> = [
      ['new', 'A'],
      ['modified', 'M'],
      ['deleted', 'D'],
      ['renamed', 'R'],
      ['typechange', 'T'],
      ['none', '·'],
    ];
    for (const [state, glyph] of cases) {
      expect(glyphFor(entry('a', { index: state }), 'staged')).toBe(glyph);
    }
  });

  it('reads the worktree axis for the changed group and the index axis for staged', () => {
    // The same entry shows a different glyph in each group — that is the point of
    // keeping two axes.
    const mm = entry('a', { index: 'new', worktree: 'deleted' });
    expect(glyphFor(mm, 'staged')).toBe('A');
    expect(glyphFor(mm, 'changed')).toBe('D');
  });

  it('marks untracked with ? and conflicted with U', () => {
    // Untracked is `worktree: 'new'`, but git writes it `??` — `A` would mean
    // "added to the index", the opposite of untracked.
    expect(glyphFor(entry('a', { worktree: 'new' }), 'untracked')).toBe('?');
    expect(glyphFor(entry('a', { conflicted: true }), 'conflicted')).toBe('U');
  });
});

describe('comparePaths', () => {
  it('keeps a directory together instead of sorting on the raw string', () => {
    // `-` sorts before `/`, so a plain string compare interleaves `src-gen/` into
    // the middle of `src/`.
    const sorted = ['src/b.ts', 'src-gen/a.ts', 'src/a.ts']
      .map((path) => ({ path }))
      .sort(comparePaths)
      .map((e) => e.path);
    expect(sorted).toEqual(['src/a.ts', 'src/b.ts', 'src-gen/a.ts']);
  });

  it('puts a file before a directory at the same level', () => {
    const sorted = ['src/nested/a.ts', 'src/a.ts']
      .map((path) => ({ path }))
      .sort(comparePaths)
      .map((e) => e.path);
    expect(sorted).toEqual(['src/a.ts', 'src/nested/a.ts']);
  });

  it('ignores case but stays deterministic on a case-only difference', () => {
    // Both files can exist at once on a case-sensitive filesystem, so the order
    // has to be defined rather than left to sort stability.
    const sorted = ['b.md', 'A.md'].map((path) => ({ path })).sort(comparePaths);
    expect(sorted.map((e) => e.path)).toEqual(['A.md', 'b.md']);

    expect(comparePaths({ path: 'README' }, { path: 'readme' })).toBeLessThan(0);
    expect(comparePaths({ path: 'readme' }, { path: 'README' })).toBeGreaterThan(0);
    expect(comparePaths({ path: 'a' }, { path: 'a' })).toBe(0);
  });
});

describe('nonEmptyGroups', () => {
  it('drops empty groups, which is what the panel renders', () => {
    // C46. `groupEntries` keeps them so its answer is complete; the caller
    // decides not to draw an empty header.
    const groups = groupEntries([entry('a.txt', { worktree: 'modified' })]);
    expect(groups).toHaveLength(4);
    expect(nonEmptyGroups(groups).map((g) => g.id)).toEqual(['changed']);
    expect(nonEmptyGroups(groupEntries([]))).toEqual([]);
  });
});

describe('hasStagedWork', () => {
  it('is true only for a non-conflicted staged entry', () => {
    expect(hasStagedWork([entry('a', { index: 'modified' })])).toBe(true);
    expect(hasStagedWork([entry('a', { worktree: 'modified' })])).toBe(false);
    // Committing mid-conflict is possible in git but records the markers, so the
    // panel treats resolving as the next step instead of offering a commit.
    expect(hasStagedWork([entry('a', { conflicted: true, index: 'modified' })])).toBe(false);
    expect(hasStagedWork([])).toBe(false);
  });
});

describe('summarize', () => {
  it('names only the groups that have rows', () => {
    const text = summarize([
      entry('a.txt', { index: 'modified' }),
      entry('b.txt', { worktree: 'modified' }),
      entry('c.txt', { worktree: 'new' }),
    ]);
    expect(text).toBe('1 đã stage · 1 thay đổi · 1 chưa theo dõi');
    expect(summarize([])).toBe('');
  });

  it('counts an MM file once per group it appears in', () => {
    // Consistent with C42: the summary reflects the rows, so `1 · 1` matches what
    // the list shows rather than claiming one change.
    expect(summarize([entry('a.txt', { index: 'modified', worktree: 'modified' })])).toBe(
      '1 đã stage · 1 thay đổi',
    );
  });
});
