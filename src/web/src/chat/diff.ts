// Line diff for `ToolCallContent::Diff` (SPEC-004 §5.1, [SPEC-004 INVENTED-3]).
//
// Hand-rolled rather than `@codemirror/merge`: that package is an editor
// extension — it wants a mounted view, a state and a theme — and a tool-call card
// needs a read-only list of coloured lines. It is also several hundred KB that
// SPEC-005 will load anyway for the real merge editor; paying for it in every chat
// bubble is the wrong trade.
//
// Plain LCS over whole lines. O(n*m) memory, which is why `MAX_LINES` exists: an
// agent rewriting a 50k-line file would otherwise allocate a 2.5-billion-cell
// table and hang the tab.

/** One row of a rendered diff. */
export interface DiffLine {
  type: 'context' | 'add' | 'remove';
  text: string;
  /** 1-based line number in the old file, or null for an added line. */
  oldLine: number | null;
  /** 1-based line number in the new file, or null for a removed line. */
  newLine: number | null;
}

export interface DiffResult {
  lines: DiffLine[];
  added: number;
  removed: number;
  /** True when the inputs were too large and the diff fell back to whole-file. */
  truncated: boolean;
}

/**
 * Above this, LCS is abandoned for a plain "all removed, all added" rendering.
 *
 * 2000×2000 is 4M cells — tens of MB and still fast enough to feel instant. A
 * file that big in a chat card is already unreadable, so precision buys nothing.
 */
const MAX_LINES = 2000;

/**
 * Diff two file versions by line.
 *
 * `oldText` is `null`/`undefined` for a newly created file — ACP models that
 * explicitly (`Diff.old_text: Option<String>`), and treating it as `""` is right:
 * every line is an addition.
 */
export function lineDiff(oldText: string | null | undefined, newText: string): DiffResult {
  const before = splitLines(oldText ?? '');
  const after = splitLines(newText);

  if (before.length > MAX_LINES || after.length > MAX_LINES) {
    return wholeFile(before, after);
  }

  const lcs = lcsTable(before, after);
  const lines: DiffLine[] = [];
  let added = 0;
  let removed = 0;

  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      lines.push({ type: 'context', text: before[i]!, oldLine: i + 1, newLine: j + 1 });
      i++;
      j++;
    } else if (lcs[i + 1]![j]! >= lcs[i]![j + 1]!) {
      // Removals before additions at the same position: a changed line then reads
      // as "- old" immediately above "+ new", which is how every diff tool shows
      // it and what the eye expects.
      lines.push({ type: 'remove', text: before[i]!, oldLine: i + 1, newLine: null });
      removed++;
      i++;
    } else {
      lines.push({ type: 'add', text: after[j]!, oldLine: null, newLine: j + 1 });
      added++;
      j++;
    }
  }
  while (i < before.length) {
    lines.push({ type: 'remove', text: before[i]!, oldLine: i + 1, newLine: null });
    removed++;
    i++;
  }
  while (j < after.length) {
    lines.push({ type: 'add', text: after[j]!, oldLine: null, newLine: j + 1 });
    added++;
    j++;
  }

  return { lines, added, removed, truncated: false };
}

/**
 * Split into lines without inventing a trailing empty one.
 *
 * `"a\n".split('\n')` is `['a', '']`, which would render a phantom blank line at
 * the end of every well-formed file.
 */
function splitLines(text: string): string[] {
  if (text === '') return [];
  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

/**
 * `lcs[i][j]` = length of the longest common subsequence of `a[i..]` and `b[j..]`.
 *
 * Built backwards so the walk above can move forwards, which keeps the output in
 * file order without a reversal step.
 */
function lcsTable(a: string[], b: string[]): number[][] {
  const table: number[][] = Array.from({ length: a.length + 1 }, () =>
    new Array<number>(b.length + 1).fill(0),
  );
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      table[i]![j] = a[i] === b[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
    }
  }
  return table;
}

/** Fallback for oversized inputs: no alignment attempted. */
function wholeFile(before: string[], after: string[]): DiffResult {
  const lines: DiffLine[] = [
    ...before.map((text, i): DiffLine => ({ type: 'remove', text, oldLine: i + 1, newLine: null })),
    ...after.map((text, i): DiffLine => ({ type: 'add', text, oldLine: null, newLine: i + 1 })),
  ];
  return { lines, added: after.length, removed: before.length, truncated: true };
}
