import { describe, it, expect } from 'vitest';
import { highlight, highlightMatch, normalizeRanges, commonIndent } from './highlight';

/** Concatenating the segments must always reproduce the input — the invariant. */
function joined(text: string, ranges: Array<[number, number]>): string {
  return highlight(text, ranges)
    .map((s) => s.text)
    .join('');
}

describe('highlight', () => {
  it('marks a range in the middle', () => {
    expect(highlight('a needle b', [[2, 8]])).toEqual([
      { text: 'a ', match: false },
      { text: 'needle', match: true },
      { text: ' b', match: false },
    ]);
  });

  it('handles a range at the very start (D38)', () => {
    expect(highlight('needle tail', [[0, 6]])).toEqual([
      { text: 'needle', match: true },
      { text: ' tail', match: false },
    ]);
  });

  it('handles a range at the very end (D38)', () => {
    expect(highlight('head needle', [[5, 11]])).toEqual([
      { text: 'head ', match: false },
      { text: 'needle', match: true },
    ]);
  });

  it('handles a range covering the whole line', () => {
    expect(highlight('needle', [[0, 6]])).toEqual([{ text: 'needle', match: true }]);
  });

  it('merges overlapping ranges instead of duplicating text (D38)', () => {
    // A regex alternation can report both. Emitting one segment per range would
    // render "abcdefgh" as "abcde" + "defgh" — the shared "de" twice.
    const segments = highlight('abcdefgh', [
      [0, 5],
      [3, 8],
    ]);
    expect(segments).toEqual([{ text: 'abcdefgh', match: true }]);
    expect(joined('abcdefgh', [
      [0, 5],
      [3, 8],
    ])).toBe('abcdefgh');
  });

  it('merges touching ranges into one mark', () => {
    expect(highlight('abcdef', [
      [0, 3],
      [3, 6],
    ])).toEqual([{ text: 'abcdef', match: true }]);
  });

  it('accepts ranges out of order', () => {
    expect(highlight('a-b-c', [
      [4, 5],
      [0, 1],
    ])).toEqual([
      { text: 'a', match: true },
      { text: '-b-', match: false },
      { text: 'c', match: true },
    ]);
  });

  it('treats ranges as byte offsets, not UTF-16 indices', () => {
    // "café " is 6 bytes but 5 JS characters. The server's needle starts at byte
    // 6; a naive `text.slice(6, 12)` would return "eedle " — off by one.
    const text = 'café needle';
    const segments = highlight(text, [[6, 12]]);
    expect(segments.find((s) => s.match)?.text).toBe('needle');
    expect(segments.map((s) => s.text).join('')).toBe(text);
  });

  it('handles a multi-byte character inside the match', () => {
    const text = 'a café b';
    // "café" spans bytes 2..7 (c,a,f + 2-byte é).
    expect(highlight(text, [[2, 7]])).toEqual([
      { text: 'a ', match: false },
      { text: 'café', match: true },
      { text: ' b', match: false },
    ]);
  });

  it('returns one plain segment when ranges are empty (non-UTF-8 line, §3.1)', () => {
    expect(highlight('caf� needle', [])).toEqual([
      { text: 'caf� needle', match: false },
    ]);
  });

  it('returns nothing for an empty line', () => {
    expect(highlight('', [[0, 3]])).toEqual([]);
  });

  it('clamps ranges past the end rather than producing undefined text', () => {
    expect(highlight('abc', [[1, 99]])).toEqual([
      { text: 'a', match: false },
      { text: 'bc', match: true },
    ]);
  });

  it('drops degenerate and negative ranges', () => {
    expect(highlight('abc', [
      [2, 2],
      [-5, -1],
    ])).toEqual([{ text: 'abc', match: false }]);
  });

  it('always reproduces the input when concatenated', () => {
    const text = 'const needle = NEEDLE + needle;';
    for (const ranges of [
      [[6, 12]],
      [[6, 12], [15, 21]],
      [[0, 5], [4, 12]],
      [[24, 30]],
      [],
    ] as Array<Array<[number, number]>>) {
      expect(joined(text, ranges)).toBe(text);
    }
  });
});

describe('normalizeRanges', () => {
  it('sorts, merges, and clamps in one pass', () => {
    expect(
      normalizeRanges(
        [
          [5, 9],
          [0, 3],
          [2, 6],
          [20, 40],
        ],
        10,
      ),
    ).toEqual([[0, 9]]);
  });
});

describe('highlightMatch', () => {
  it('reads text and ranges off a streamed match', () => {
    expect(
      highlightMatch({ path: 'a.ts', line: 3, text: 'x needle', ranges: [[2, 8]] }),
    ).toEqual([
      { text: 'x ', match: false },
      { text: 'needle', match: true },
    ]);
  });
});

describe('commonIndent', () => {
  it('ignores blank lines when computing the shared indent', () => {
    expect(commonIndent(['    a', '', '      b'])).toBe(4);
  });

  it('is zero when any line is flush left, and for no lines at all', () => {
    expect(commonIndent(['a', '    b'])).toBe(0);
    expect(commonIndent([])).toBe(0);
    expect(commonIndent(['', '  '])).toBe(0);
  });
});
