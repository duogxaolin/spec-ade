// Collapsing runs of tool calls into one card (SPEC-004 §5.1, [SPEC-004 INVENTED-1]).
//
// A single edit turn can emit a dozen `tool_call`s between two sentences. Listed
// one per line they bury the reply the user is actually reading, so adjacent calls
// become one "✓ 3 tool calls" row that expands.
//
// Grouping happens here, over the transcript, and NOT in `stores/acpSession.ts`:
// the fold owns protocol meaning (sparse patches, plan replacement) and is covered
// by SPEC-003's tests. How many rows a run of calls occupies on screen is a
// rendering decision, and mixing it into the fold would make a display tweak a
// protocol change.

import type { TranscriptEntry } from '../stores/acpSession';

/** A transcript row: either one entry, or a run of tool calls shown as one. */
export type ChatRow =
  | { kind: 'entry'; key: string; entry: TranscriptEntry }
  | { kind: 'toolGroup'; key: string; toolCallIds: string[]; seq: number };

/**
 * Fold adjacent `tool` entries into groups, leaving everything else alone.
 *
 * Adjacency is the grouping key rather than time or tool kind: a message between
 * two calls means the agent said something about the first batch, so merging
 * across it would attach the explanation to the wrong calls.
 */
export function groupEntries(entries: readonly TranscriptEntry[]): ChatRow[] {
  const rows: ChatRow[] = [];

  for (const entry of entries) {
    if (entry.kind !== 'tool') {
      rows.push({ kind: 'entry', key: rowKey(entry), entry });
      continue;
    }

    const last = rows[rows.length - 1];
    if (last?.kind === 'toolGroup') {
      // A `tool_call_update` for a call already in this group must not add a
      // second id — the fold dedupes entries, but a gap can replay one.
      if (!last.toolCallIds.includes(entry.toolCallId)) {
        last.toolCallIds.push(entry.toolCallId);
      }
      continue;
    }

    rows.push({
      kind: 'toolGroup',
      // Keyed by the first call's seq: stable as the group grows, so Vue does not
      // tear down and rebuild the card on every added call.
      key: `tools-${entry.seq}`,
      toolCallIds: [entry.toolCallId],
      seq: entry.seq,
    });
  }

  return rows;
}

/**
 * Whether a group starts collapsed.
 *
 * One call is its own row and hiding it behind a disclosure would cost a click for
 * nothing; two or more is where the noise begins.
 */
export function startsCollapsed(group: { toolCallIds: string[] }): boolean {
  return group.toolCallIds.length >= 2;
}

/** `v-for` key. `seq` alone can repeat across kinds after a replay. */
function rowKey(entry: TranscriptEntry): string {
  return `${entry.kind}-${entry.seq}`;
}
