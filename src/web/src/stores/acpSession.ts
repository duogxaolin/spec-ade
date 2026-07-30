// Folding an ACP event stream into something renderable (SPEC-003 §5.8).
//
// Pure on purpose: the fold is where the protocol's sharp edges live (sparse
// tool-call patches, whole-plan replacement, chunk coalescing), and a plain
// function over a plain object can be tested without a socket or a Pinia
// instance. The store in `acp.ts` owns the reactivity and the sockets; this owns
// the meaning of each event.

import type {
  AcpServerMessage,
  AcpSessionState,
  PermissionOptionView,
  PlanPayload,
  StopReason,
  ToolCallPayload,
  ToolCallPatch,
} from '../api/acp';
import { stopReasonLabel } from '../api/acpSocket';

/**
 * One block in the transcript, in arrival order.
 *
 * Tool calls are referenced by id rather than embedded so a later
 * `tool_call_update` mutates one object instead of having to find and rewrite an
 * array element.
 */
export type TranscriptEntry =
  | { kind: 'message'; seq: number; text: string }
  | { kind: 'thought'; seq: number; text: string }
  | { kind: 'tool'; seq: number; toolCallId: string }
  | { kind: 'turn_end'; seq: number; stopReason: StopReason; label: string }
  | { kind: 'gap'; seq: number; fromSeq: number }
  | { kind: 'notice'; seq: number; text: string };

/** A parked permission request: the turn is blocked until this is answered. */
export interface PendingPermission {
  requestId: string;
  toolCall: ToolCallPatch;
  options: PermissionOptionView[];
}

/** Everything one session's UI needs, derived entirely from its events. */
export interface SessionView {
  entries: TranscriptEntry[];
  /** Keyed by `toolCallId`; entries of kind `tool` point in here. */
  toolCalls: Record<string, ToolCallPayload>;
  /** Latest full snapshot — never merged, always replaced. */
  plan: PlanPayload | null;
  usage: Record<string, unknown> | null;
  mode: Record<string, unknown> | null;
  permission: PendingPermission | null;
  state: AcpSessionState;
  /** True between a `prompt` and its `turn_complete`. */
  turnActive: boolean;
  /** Highest `seq` folded in — the resume cursor after a reload. */
  lastSeq: number;
  /** History was pruned: the transcript has holes (A13). */
  hasGap: boolean;
}

export function createSessionView(state: AcpSessionState = 'idle'): SessionView {
  return {
    entries: [],
    toolCalls: {},
    plan: null,
    usage: null,
    mode: null,
    permission: null,
    state,
    turnActive: state === 'prompting',
    lastSeq: 0,
    hasGap: false,
  };
}

/**
 * Fold one event into `view`, mutating it in place.
 *
 * In place rather than returning a new view: the store hands out a reactive
 * object that components already hold, and message chunks arrive per token —
 * rebuilding the whole transcript on each one would re-render the pane hundreds
 * of times a turn.
 */
export function applyAcpEvent(view: SessionView, event: AcpServerMessage & { seq: number }): void {
  // `truncated` reports a gap *at* the next unseen seq, so it must not advance
  // the cursor past the event it points at.
  if (event.type !== 'truncated' && event.seq > view.lastSeq) {
    view.lastSeq = event.seq;
  }

  switch (event.type) {
    case 'message_chunk':
      appendText(view, 'message', event.seq, event.text);
      break;

    // Kept as its own block kind rather than mixed into the reply: reasoning is
    // the agent thinking out loud, and splicing it into the answer would put
    // words in the reply the agent never addressed to the user.
    case 'thought_chunk':
      appendText(view, 'thought', event.seq, event.text);
      break;

    case 'tool_call': {
      const call = event.toolCall;
      view.toolCalls[call.toolCallId] = { ...view.toolCalls[call.toolCallId], ...prune(call) };
      if (!view.entries.some((e) => e.kind === 'tool' && e.toolCallId === call.toolCallId)) {
        view.entries.push({ kind: 'tool', seq: event.seq, toolCallId: call.toolCallId });
      }
      break;
    }

    case 'tool_call_update': {
      const patch = event.toolCall;
      const existing = view.toolCalls[patch.toolCallId];
      // Absent fields mean *unchanged*: `prune` drops them so a status-only patch
      // can't blank the title the user is reading.
      view.toolCalls[patch.toolCallId] = { ...existing, ...prune(patch) };
      // An update for a call we never saw announced is normal after a gap — show
      // it rather than dropping the only record of what the agent did.
      if (!existing) {
        view.entries.push({ kind: 'tool', seq: event.seq, toolCallId: patch.toolCallId });
      }
      break;
    }

    // A plan is a full snapshot of the agent's current thinking. Merging would
    // resurrect steps it has since dropped.
    case 'plan':
      view.plan = event.plan;
      break;

    case 'usage':
      view.usage = event.usage;
      break;

    case 'mode':
      view.mode = event.mode;
      break;

    case 'permission_request':
      view.permission = {
        requestId: event.requestId,
        toolCall: event.toolCall,
        options: event.options,
      };
      break;

    case 'permission_resolved':
      // Match on id: a stale resolution must not clear a newer request. The
      // server can resolve one we never rendered (timeout, or another tab).
      if (view.permission?.requestId === event.requestId) {
        view.permission = null;
      }
      view.entries.push({
        kind: 'notice',
        seq: event.seq,
        text: `permission ${event.outcome}`,
      });
      break;

    case 'turn_complete':
      view.turnActive = false;
      view.entries.push({
        kind: 'turn_end',
        seq: event.seq,
        stopReason: event.stopReason,
        label: stopReasonLabel(event.stopReason),
      });
      break;

    case 'session_state':
      view.state = event.state;
      view.turnActive = event.state === 'prompting';
      break;

    case 'connection_closed':
      view.state = 'closed';
      view.turnActive = false;
      // A parked request can never be answered now; leaving the buttons on
      // screen would invite a click that goes nowhere.
      view.permission = null;
      view.entries.push({
        kind: 'notice',
        seq: event.seq,
        text: `agent stopped: ${event.reason}`,
      });
      break;

    case 'truncated':
      view.hasGap = true;
      view.entries.push({ kind: 'gap', seq: event.seq, fromSeq: event.fromSeq });
      break;

    // `ready`, `pong` and `error` never reach here — the socket handles them and
    // they carry no place in the transcript.
    default:
      break;
  }
}

/**
 * Append to the trailing block of `kind`, or start a new one.
 *
 * Chunks arrive per token; one entry per chunk would make a reply a list of
 * fragments that no `white-space` rule could reassemble correctly.
 */
function appendText(
  view: SessionView,
  kind: 'message' | 'thought',
  seq: number,
  text: string,
): void {
  const last = view.entries[view.entries.length - 1];
  if (last && last.kind === kind) {
    last.text += text;
    return;
  }
  view.entries.push({ kind, seq, text });
}

/** Drop `undefined` values so a spread can't overwrite known fields with them. */
function prune(patch: ToolCallPatch): Partial<ToolCallPayload> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(patch)) {
    if (value !== undefined) out[key] = value;
  }
  return out as Partial<ToolCallPayload>;
}

/** The whole reply so far, for tests and for copy-to-clipboard. */
export function messageText(view: SessionView): string {
  return view.entries
    .filter((e): e is TranscriptEntry & { kind: 'message' } => e.kind === 'message')
    .map((e) => e.text)
    .join('');
}
