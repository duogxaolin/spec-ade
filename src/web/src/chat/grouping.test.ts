import { describe, expect, it } from 'vitest';

import type { TranscriptEntry } from '../stores/acpSession';
import { groupEntries, startsCollapsed } from './grouping';

// SPEC-004 B12-B14. Grouping is a rendering decision over the SPEC-003 fold, so
// these tests build TranscriptEntry values directly instead of replaying events.

let seq = 0;
function message(text = 'hi'): TranscriptEntry {
  return { kind: 'message', seq: ++seq, text };
}
function tool(toolCallId: string): TranscriptEntry {
  return { kind: 'tool', seq: ++seq, toolCallId };
}
function turnEnd(): TranscriptEntry {
  return { kind: 'turn_end', seq: ++seq, stopReason: 'end_turn', label: '' };
}

describe('groupEntries', () => {
  it('returns nothing for an empty transcript', () => {
    expect(groupEntries([])).toEqual([]);
  });

  it('passes non-tool entries through one row each', () => {
    const rows = groupEntries([message('a'), message('b'), turnEnd()]);
    expect(rows).toHaveLength(3);
    expect(rows.every((r) => r.kind === 'entry')).toBe(true);
  });

  it('folds a run of adjacent tool calls into one row', () => {
    const rows = groupEntries([tool('t1'), tool('t2'), tool('t3')]);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ kind: 'toolGroup', toolCallIds: ['t1', 't2', 't3'] });
  });

  // The reason adjacency is the key: a sentence between two batches explains the
  // first one, so merging across it would attach the text to the wrong calls.
  it('does not merge across a message', () => {
    const rows = groupEntries([tool('t1'), message('xong bước 1'), tool('t2')]);
    expect(rows.map((r) => r.kind)).toEqual(['toolGroup', 'entry', 'toolGroup']);
  });

  it('does not merge across a turn end', () => {
    const rows = groupEntries([tool('t1'), turnEnd(), tool('t2')]);
    expect(rows).toHaveLength(3);
  });

  it('dedupes a tool id replayed inside the same run', () => {
    const rows = groupEntries([tool('t1'), tool('t1'), tool('t2')]);
    expect(rows[0]).toMatchObject({ toolCallIds: ['t1', 't2'] });
  });

  it('keys a group by its first seq, so it survives growing', () => {
    const first = tool('t1');
    const before = groupEntries([first]);
    const after = groupEntries([first, tool('t2')]);
    expect(after[0]!.key).toBe(before[0]!.key);
    expect(after[0]!.key).toBe(`tools-${first.seq}`);
  });

  it('gives entry rows a key that is unique across kinds at the same seq', () => {
    // A replay can reuse a seq across kinds; the key must still differ.
    const rows = groupEntries([
      { kind: 'message', seq: 7, text: 'a' },
      { kind: 'thought', seq: 7, text: 'b' },
    ]);
    expect(rows[0]!.key).not.toBe(rows[1]!.key);
  });

  it('keeps entry order stable', () => {
    const rows = groupEntries([message('1'), tool('t1'), message('2')]);
    expect(rows.map((r) => (r.kind === 'entry' ? r.entry.kind : 'toolGroup'))).toEqual([
      'message',
      'toolGroup',
      'message',
    ]);
  });

  it('handles gap and notice entries as ordinary rows', () => {
    const rows = groupEntries([
      { kind: 'gap', seq: 1, fromSeq: 40 },
      { kind: 'notice', seq: 2, text: 'mất kết nối' },
    ]);
    expect(rows).toHaveLength(2);
    expect(rows.every((r) => r.kind === 'entry')).toBe(true);
  });

  it('does not mutate the input array', () => {
    const entries = [tool('t1'), tool('t2')];
    const snapshot = JSON.stringify(entries);
    groupEntries(entries);
    expect(JSON.stringify(entries)).toBe(snapshot);
  });
});

describe('startsCollapsed', () => {
  it('leaves a single call open — a disclosure would cost a click for nothing', () => {
    expect(startsCollapsed({ toolCallIds: ['t1'] })).toBe(false);
  });

  it('collapses two or more', () => {
    expect(startsCollapsed({ toolCallIds: ['t1', 't2'] })).toBe(true);
    expect(startsCollapsed({ toolCallIds: ['t1', 't2', 't3'] })).toBe(true);
  });
});
