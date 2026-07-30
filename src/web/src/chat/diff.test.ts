import { describe, expect, it } from 'vitest';

import { lineDiff } from './diff';

// SPEC-004 B16-B18. Pure computation, node environment.

/** Compact rendering of a diff, so expectations read like a patch. */
function render(oldText: string | null, newText: string): string[] {
  return lineDiff(oldText, newText).lines.map((l) => {
    const sign = l.type === 'add' ? '+' : l.type === 'remove' ? '-' : ' ';
    return `${sign}${l.text}`;
  });
}

describe('lineDiff', () => {
  it('reports no changes for identical text', () => {
    const result = lineDiff('a\nb\n', 'a\nb\n');
    expect(result.added).toBe(0);
    expect(result.removed).toBe(0);
    expect(result.lines.every((l) => l.type === 'context')).toBe(true);
  });

  it('treats a null old text as a new file: everything is an addition', () => {
    const result = lineDiff(null, 'a\nb\n');
    expect(result.removed).toBe(0);
    expect(result.added).toBe(2);
    expect(result.lines.every((l) => l.type === 'add')).toBe(true);
  });

  it('treats undefined the same as null', () => {
    expect(lineDiff(undefined, 'x\n').added).toBe(1);
  });

  it('reports a deletion when the new text is empty', () => {
    const result = lineDiff('a\nb\n', '');
    expect(result.removed).toBe(2);
    expect(result.added).toBe(0);
  });

  it('finds a pure insertion in the middle', () => {
    expect(render('a\nc\n', 'a\nb\nc\n')).toEqual([' a', '+b', ' c']);
  });

  it('finds a pure deletion in the middle', () => {
    expect(render('a\nb\nc\n', 'a\nc\n')).toEqual([' a', '-b', ' c']);
  });

  // The ordering rule: a changed line must read as "- old" directly above "+ new".
  it('emits the removal before the addition for a changed line', () => {
    expect(render('a\nold\nc\n', 'a\nnew\nc\n')).toEqual([' a', '-old', '+new', ' c']);
  });

  it('numbers lines against the correct side', () => {
    const { lines } = lineDiff('a\nold\nc\n', 'a\nnew\nc\n');
    const removal = lines.find((l) => l.type === 'remove')!;
    const addition = lines.find((l) => l.type === 'add')!;
    expect(removal).toMatchObject({ oldLine: 2, newLine: null });
    expect(addition).toMatchObject({ oldLine: null, newLine: 2 });
    // Context lines carry both, and the last one is line 3 on both sides.
    expect(lines[lines.length - 1]).toMatchObject({ oldLine: 3, newLine: 3 });
  });

  it('does not invent a trailing blank line for text ending in a newline', () => {
    expect(lineDiff('a\n', 'a\n').lines).toHaveLength(1);
  });

  it('keeps a genuine trailing blank line', () => {
    // "a\n\n" is two lines: "a" and "". Only the phantom one is dropped.
    expect(lineDiff('a\n\n', 'a\n\n').lines).toHaveLength(2);
  });

  it('handles text with no trailing newline', () => {
    expect(render('a\nb', 'a\nb\nc')).toEqual([' a', ' b', '+c']);
  });

  it('aligns a moved block instead of rewriting everything', () => {
    // LCS should keep the long common run as context.
    const result = lineDiff('h\n1\n2\n3\nf\n', 'h\nx\n1\n2\n3\nf\n');
    expect(result.added).toBe(1);
    expect(result.removed).toBe(0);
  });

  it('reports both counts for a mixed edit', () => {
    const result = lineDiff('a\nb\nc\n', 'a\nB\nc\nd\n');
    expect(result.added).toBe(2);
    expect(result.removed).toBe(1);
    expect(result.truncated).toBe(false);
  });

  it('preserves indentation and empty lines verbatim', () => {
    const result = lineDiff('fn a() {\n\n    x\n}\n', 'fn a() {\n\n    y\n}\n');
    expect(result.lines.map((l) => l.text)).toContain('    x');
    expect(result.lines.map((l) => l.text)).toContain('    y');
  });

  // The guard that keeps a 50k-line rewrite from allocating a billion cells.
  it('falls back to whole-file above the line cap', () => {
    const big = `${Array.from({ length: 2001 }, (_, i) => `line ${i}`).join('\n')}\n`;
    const result = lineDiff(big, `${big}extra\n`);
    expect(result.truncated).toBe(true);
    expect(result.removed).toBe(2001);
    expect(result.added).toBe(2002);
    // No context lines in the fallback: nothing was aligned.
    expect(result.lines.some((l) => l.type === 'context')).toBe(false);
  });

  it('stays exact right up to the cap', () => {
    const lines = Array.from({ length: 2000 }, (_, i) => `line ${i}`).join('\n');
    const result = lineDiff(`${lines}\n`, `${lines}\n`);
    expect(result.truncated).toBe(false);
    expect(result.added).toBe(0);
  });

  it('handles two empty inputs without producing rows', () => {
    expect(lineDiff('', '')).toEqual({ lines: [], added: 0, removed: 0, truncated: false });
  });
});
