import { describe, expect, it } from 'vitest';

import { fencesBalanced, openFence, splitStreamingTail } from './fences';

// SPEC-004 B7-B9: the streaming-tail split. These are pure string rules, so they
// run in the default node environment — no DOM needed.

describe('openFence', () => {
  it('returns null for text with no fence at all', () => {
    expect(openFence('just a paragraph\nand another')).toBeNull();
  });

  it('returns null when the fence is closed', () => {
    expect(openFence('```rust\nfn main() {}\n```\ndone')).toBeNull();
  });

  it('finds an unterminated fence and reports its info string', () => {
    const open = openFence('intro\n```rust\nfn ma');
    expect(open).not.toBeNull();
    expect(open!.info).toBe('rust');
    expect(open!.marker).toBe('```');
    expect(open!.start).toBe('intro\n'.length);
  });

  it('treats an opener with no trailing newline as open', () => {
    const open = openFence('```mermaid');
    expect(open?.info).toBe('mermaid');
  });

  it('keeps the full info string, not just the language', () => {
    expect(openFence('```ts title="a.ts"')?.info).toBe('ts title="a.ts"');
  });

  // CommonMark: the closer must be at least as long as the opener, which is how a
  // 4-backtick fence quotes a 3-backtick one verbatim.
  it('does not let a shorter fence close a longer one', () => {
    const open = openFence('````\n```\nstill inside\n');
    expect(open?.marker).toBe('````');
  });

  it('lets a longer fence close a shorter one', () => {
    expect(openFence('```\ncode\n````\n')).toBeNull();
  });

  it('does not let tildes close backticks', () => {
    expect(openFence('```\ncode\n~~~\n')).not.toBeNull();
  });

  it('supports tilde fences', () => {
    expect(openFence('~~~\ncode\n~~~\n')).toBeNull();
    expect(openFence('~~~\ncode\n')).not.toBeNull();
  });

  it('allows up to three spaces of indentation', () => {
    expect(openFence('   ```\ncode\n   ```\n')).toBeNull();
  });

  // Four spaces is an indented code block, not a fence — so the backticks are
  // literal content and there is nothing to close.
  it('ignores a fence indented four spaces', () => {
    expect(openFence('    ```\ncode\n')).toBeNull();
  });

  it('ignores a closing fence that carries an info string', () => {
    expect(openFence('```\ncode\n``` trailing\n')).not.toBeNull();
  });

  it('rejects a backtick opener whose info contains a backtick', () => {
    // `` ```a`b `` is not a valid opener, so this line is a paragraph.
    expect(openFence('```a`b\ntext')).toBeNull();
  });

  it('handles two closed fences in a row', () => {
    expect(openFence('```a\n1\n```\ntext\n```b\n2\n```')).toBeNull();
  });

  it('reports the second fence when only it is open', () => {
    const open = openFence('```a\n1\n```\ntext\n```b\n2');
    expect(open?.info).toBe('b');
  });

  it('ignores inline code spans', () => {
    expect(openFence('use `cargo build` then stop')).toBeNull();
  });

  it('is stateless across calls, so replayed text gives the same answer', () => {
    const text = '```rust\nfn ma';
    expect(openFence(text)).toEqual(openFence(text));
  });
});

describe('fencesBalanced', () => {
  it('is true only when nothing is open', () => {
    expect(fencesBalanced('```\nx\n```')).toBe(true);
    expect(fencesBalanced('```\nx')).toBe(false);
  });
});

describe('splitStreamingTail', () => {
  it('passes everything through as stable when no fence is open', () => {
    const { stable, tail } = splitStreamingTail('# Title\n\ntext');
    expect(stable).toBe('# Title\n\ntext');
    expect(tail).toBeNull();
  });

  it('splits the prose from the open fence body', () => {
    const { stable, tail } = splitStreamingTail('Here you go:\n\n```rust\nfn main() {\n');
    expect(stable).toBe('Here you go:\n\n');
    expect(tail?.info).toBe('rust');
    expect(tail?.code).toBe('fn main() {\n');
  });

  it('yields an empty body when only the opener has arrived', () => {
    const { stable, tail } = splitStreamingTail('```rust');
    expect(stable).toBe('');
    expect(tail?.code).toBe('');
  });

  // The point of the split: as the fence closes, the prose half must not change.
  // If it did, the block would visibly re-flow on the last chunk.
  it('keeps the stable half byte-identical as the fence fills in', () => {
    const prefix = 'Reply:\n\n';
    const growing = ['```ts\n', '```ts\nlet a', '```ts\nlet a = 1;\n'];
    for (const step of growing) {
      expect(splitStreamingTail(prefix + step).stable).toBe(prefix);
    }
    // Once closed, the whole thing is stable and markdown-it takes over.
    const closed = splitStreamingTail(`${prefix}\`\`\`ts\nlet a = 1;\n\`\`\``);
    expect(closed.tail).toBeNull();
    expect(closed.stable).toBe(`${prefix}\`\`\`ts\nlet a = 1;\n\`\`\``);
  });
});
