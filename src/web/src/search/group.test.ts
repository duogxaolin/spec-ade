import { describe, it, expect } from 'vitest';
import { groupByFile, pushMatch, countMatches, fileName, dirName } from './group';
import type { SearchMatch } from '../api/search';

function m(path: string, line: number): SearchMatch {
  return { path, line, text: `line ${line}`, ranges: [] };
}

describe('groupByFile', () => {
  it('keeps files in first-appearance order, not alphabetical (D37)', () => {
    // Deliberately reverse-alphabetical, so an accidental sort is visible.
    const groups = groupByFile([m('z.ts', 1), m('a.ts', 1), m('m.ts', 1)]);
    expect(groups.map((g) => g.path)).toEqual(['z.ts', 'a.ts', 'm.ts']);
  });

  it('merges interleaved matches back into one group per file (D37)', () => {
    // This is the shape a parallel walk actually produces: file A, file B, file A.
    const groups = groupByFile([m('a.ts', 1), m('b.ts', 7), m('a.ts', 4), m('b.ts', 2)]);
    expect(groups.map((g) => g.path)).toEqual(['a.ts', 'b.ts']);
    expect(groups[0].matches.map((x) => x.line)).toEqual([1, 4]);
    expect(groups[1].matches.map((x) => x.line)).toEqual([7, 2]);
  });

  it('preserves arrival order within a file even when line numbers are not sorted', () => {
    // The walker reads a file top-to-bottom, but this asserts we never sort:
    // if a future change reorders, the group would come back as [2, 9].
    const groups = groupByFile([m('a.ts', 9), m('a.ts', 2)]);
    expect(groups[0].matches.map((x) => x.line)).toEqual([9, 2]);
  });

  it('returns nothing for no matches', () => {
    expect(groupByFile([])).toEqual([]);
  });
});

describe('pushMatch', () => {
  it('builds the same grouping incrementally as groupByFile does at once', () => {
    const stream = [m('a.ts', 1), m('b.ts', 7), m('a.ts', 4), m('c.ts', 3), m('b.ts', 2)];
    const incremental = stream.reduce<ReturnType<typeof groupByFile>>(
      (acc, one) => pushMatch(acc, one),
      [],
    );
    expect(incremental).toEqual(groupByFile(stream));
  });

  it('appends to the last group without scanning, when the file repeats', () => {
    const groups = pushMatch([], m('a.ts', 1));
    pushMatch(groups, m('a.ts', 2));
    expect(groups).toHaveLength(1);
    expect(groups[0].matches.map((x) => x.line)).toEqual([1, 2]);
  });

  it('reuses a group that is no longer last', () => {
    const groups = groupByFile([m('a.ts', 1), m('b.ts', 1)]);
    pushMatch(groups, m('a.ts', 5));
    expect(groups.map((g) => g.path)).toEqual(['a.ts', 'b.ts']);
    expect(groups[0].matches).toHaveLength(2);
  });
});

describe('countMatches', () => {
  it('sums across groups, not groups themselves', () => {
    expect(countMatches(groupByFile([m('a.ts', 1), m('a.ts', 2), m('b.ts', 1)]))).toBe(3);
    expect(countMatches([])).toBe(0);
  });
});

describe('path helpers', () => {
  it('splits a nested path', () => {
    expect(fileName('src/web/main.ts')).toBe('main.ts');
    expect(dirName('src/web/main.ts')).toBe('src/web');
  });

  it('treats a root-level file as having no directory', () => {
    expect(fileName('README.md')).toBe('README.md');
    expect(dirName('README.md')).toBe('');
  });
});
