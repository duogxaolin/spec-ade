// Unit tests for the ACP event fold (SPEC-003 §5.8).
//
// Three protocol rules are easy to get backwards and expensive when you do:
// `tool_call_update` is a sparse patch (absent ≠ cleared), `plan` is a full
// replace (merging resurrects dropped steps), and a `permission_resolved` must
// only clear the request it names.

import { describe, expect, it } from 'vitest';

import type { AcpServerMessage } from '../api/acp';
import {
  applyAcpEvent,
  createSessionView,
  messageText,
  type SessionView,
} from './acpSession';

/** Fold a list of events into a fresh view. */
function fold(...events: Array<AcpServerMessage & { seq: number }>): SessionView {
  const view = createSessionView();
  for (const event of events) applyAcpEvent(view, event);
  return view;
}

let seq = 0;
/** Sequence numbers are incidental to most of these tests. */
function next(): number {
  seq += 1;
  return seq;
}

function chunk(text: string): AcpServerMessage & { seq: number } {
  return { type: 'message_chunk', seq: next(), sessionId: 's', text };
}

describe('text chunks', () => {
  it('coalesces consecutive chunks into one block', () => {
    // Chunks arrive per token; one block each would leave a reply that no
    // whitespace rule could reassemble.
    const view = fold(chunk('Hello'), chunk(', '), chunk('world'));

    expect(view.entries).toHaveLength(1);
    expect(messageText(view)).toBe('Hello, world');
  });

  it('keeps thoughts out of the reply', () => {
    const view = fold(
      chunk('answer'),
      { type: 'thought_chunk', seq: next(), sessionId: 's', text: 'thinking' },
      chunk('more'),
    );

    // Reasoning is not addressed to the user; splicing it in would put words in
    // the reply the agent never said.
    expect(messageText(view)).toBe('answermore');
    expect(view.entries.map((e) => e.kind)).toEqual(['message', 'thought', 'message']);
  });
});

describe('tool calls', () => {
  it('merges a sparse update without clearing absent fields', () => {
    const view = fold(
      {
        type: 'tool_call',
        seq: next(),
        sessionId: 's',
        toolCall: { toolCallId: 't1', title: 'Read src/main.rs', kind: 'read', status: 'pending' },
      },
      {
        type: 'tool_call_update',
        seq: next(),
        sessionId: 's',
        toolCall: { toolCallId: 't1', status: 'completed' },
      },
    );

    expect(view.toolCalls.t1).toMatchObject({
      toolCallId: 't1',
      // Would be gone if the patch were applied as a replacement.
      title: 'Read src/main.rs',
      kind: 'read',
      status: 'completed',
    });
    // One entry, not two: the same call updating is not a new call.
    expect(view.entries.filter((e) => e.kind === 'tool')).toHaveLength(1);
  });

  it('does not resurrect a field the patch explicitly omits as undefined', () => {
    const view = fold({
      type: 'tool_call',
      seq: next(),
      sessionId: 's',
      toolCall: { toolCallId: 't2', title: 'Edit' },
    });
    applyAcpEvent(view, {
      type: 'tool_call_update',
      seq: next(),
      sessionId: 's',
      toolCall: { toolCallId: 't2', title: undefined, status: 'in_progress' },
    });

    // `{title: undefined}` is JSON's absence after a round-trip; treating it as
    // a value would blank the title the user is reading.
    expect(view.toolCalls.t2!.title).toBe('Edit');
  });

  it('shows an update for a call it never saw announced', () => {
    // Normal after a pruned log: the update is the only record of what ran.
    const view = fold({
      type: 'tool_call_update',
      seq: next(),
      sessionId: 's',
      toolCall: { toolCallId: 'orphan', status: 'completed' },
    });

    expect(view.entries.filter((e) => e.kind === 'tool')).toHaveLength(1);
    expect(view.toolCalls.orphan!.status).toBe('completed');
  });
});

describe('plan', () => {
  it('replaces wholesale rather than merging', () => {
    const view = fold(
      {
        type: 'plan',
        seq: next(),
        sessionId: 's',
        plan: { entries: [{ content: 'a' }, { content: 'b' }, { content: 'c' }] },
      },
      {
        type: 'plan',
        seq: next(),
        sessionId: 's',
        plan: { entries: [{ content: 'a', status: 'completed' }] },
      },
    );

    // The agent dropped b and c; merging would keep showing steps it abandoned.
    expect(view.plan!.entries).toEqual([{ content: 'a', status: 'completed' }]);
  });
});

describe('permissions', () => {
  const request = (requestId: string): AcpServerMessage & { seq: number } => ({
    type: 'permission_request',
    seq: next(),
    sessionId: 's',
    requestId,
    toolCall: { toolCallId: 'w1', title: 'Write README.md' },
    options: [
      { optionId: 'allow-once', name: 'Allow once', kind: 'allow_once' },
      { optionId: 'reject', name: 'Reject', kind: 'reject_once' },
    ],
  });

  it('parks a request until it is resolved', () => {
    const view = fold(request('r1'));
    expect(view.permission?.requestId).toBe('r1');
    expect(view.permission?.options).toHaveLength(2);

    applyAcpEvent(view, {
      type: 'permission_resolved',
      seq: next(),
      sessionId: 's',
      requestId: 'r1',
      outcome: 'selected:allow-once',
    });
    expect(view.permission).toBeNull();
  });

  it('ignores a resolution for a different request', () => {
    // The server can resolve one this client never rendered (a timeout, or
    // another tab). Clearing blindly would hide a live prompt.
    const view = fold(request('r2'));
    applyAcpEvent(view, {
      type: 'permission_resolved',
      seq: next(),
      sessionId: 's',
      requestId: 'stale',
      outcome: 'cancelled',
    });

    expect(view.permission?.requestId).toBe('r2');
  });

  it('clears a parked request when the agent dies', () => {
    // Nothing could answer it now; leaving the buttons up invites a dead click.
    const view = fold(request('r3'), {
      type: 'connection_closed',
      seq: next(),
      sessionId: 's',
      reason: 'agent exited',
    });

    expect(view.permission).toBeNull();
    expect(view.state).toBe('closed');
    expect(view.turnActive).toBe(false);
  });
});

describe('turn lifecycle', () => {
  it('tracks the turn from session_state and turn_complete', () => {
    const view = fold({ type: 'session_state', seq: next(), sessionId: 's', state: 'prompting' });
    expect(view.turnActive).toBe(true);

    applyAcpEvent(view, {
      type: 'turn_complete',
      seq: next(),
      sessionId: 's',
      stopReason: 'refusal',
    });
    expect(view.turnActive).toBe(false);
    const end = view.entries.at(-1)!;
    // A refusal ends the turn like any other reason — it is an answer.
    expect(end).toMatchObject({ kind: 'turn_end', stopReason: 'refusal' });
  });

  it('starts a view mid-turn when the server says so', () => {
    // A client attaching to a running turn must not offer a prompt box.
    expect(createSessionView('prompting').turnActive).toBe(true);
    expect(createSessionView('idle').turnActive).toBe(false);
  });
});

describe('cursor and gaps', () => {
  it('tracks the highest seq for resume', () => {
    const view = createSessionView();
    applyAcpEvent(view, { type: 'message_chunk', seq: 4, sessionId: 's', text: 'a' });
    applyAcpEvent(view, { type: 'message_chunk', seq: 5, sessionId: 's', text: 'b' });
    expect(view.lastSeq).toBe(5);

    // Never goes backwards: a late duplicate must not rewind the resume point.
    applyAcpEvent(view, { type: 'message_chunk', seq: 3, sessionId: 's', text: 'old' });
    expect(view.lastSeq).toBe(5);
  });

  it('records a gap without advancing past the event it points at', () => {
    const view = createSessionView();
    applyAcpEvent(view, { type: 'truncated', seq: 100, sessionId: 's', fromSeq: 100 });

    expect(view.hasGap).toBe(true);
    expect(view.entries.at(-1)).toMatchObject({ kind: 'gap', fromSeq: 100 });
    // Adopting 100 would make a reconnect skip the first surviving event.
    expect(view.lastSeq).toBe(0);
  });
});

describe('usage and mode', () => {
  it('keeps only the latest of each', () => {
    const view = fold(
      { type: 'usage', seq: next(), sessionId: 's', usage: { inputTokens: 10 } },
      { type: 'usage', seq: next(), sessionId: 's', usage: { inputTokens: 25 } },
      { type: 'mode', seq: next(), sessionId: 's', mode: { currentModeId: 'ask' } },
    );

    expect(view.usage).toEqual({ inputTokens: 25 });
    expect(view.mode).toEqual({ currentModeId: 'ask' });
    // Neither belongs in the transcript.
    expect(view.entries).toHaveLength(0);
  });
});
