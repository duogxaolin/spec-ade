// ACP REST client + the shapes the WebSocket speaks (SPEC-003 §3.1–§3.2).
//
// Three ids are in play and mixing them up is the easiest way to break this:
//
// - `connectionId` — one agent *process*. Spawned per project, serves many
//   sessions ([INVENTED-1]).
// - `sessionId`    — Spec ADE's session id. The only one the browser ever sends.
// - `agentSessionId` — what the agent called the session. Reported for log
//   correlation only; never used to address anything from here.

import { apiFetch } from './client';

/** One entry of the configured agent catalogue (§3.4, read-only this phase). */
export interface AcpAgentEntry {
  id: string;
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
}

/** The 201 body of `POST /api/acp/spawn`. */
export interface AcpConnectionInfo {
  id: string;
  agentId: string;
  projectId: string;
  /** The agent's self-reported name/version, or `null` if it sent none. */
  agentInfo: { name?: string; version?: string } | null;
  /** What the agent said it supports at `initialize` — gate UI on this, not hope. */
  agentCapabilities: Record<string, unknown>;
}

/** A row of `GET /api/acp`. */
export interface AcpConnectionSummary {
  id: string;
  agentId: string;
  projectId: string;
  state: string;
  sessionCount: number;
  sessionIds: string[];
}

/** A row of `GET /api/projects/{id}/sessions`. */
export interface AcpSession {
  id: string;
  projectId: string;
  connectionId: string;
  agentSessionId: string;
  createdAt: string;
  cwd: string;
}

export function listAgents(): Promise<AcpAgentEntry[]> {
  return apiFetch<AcpAgentEntry[]>('/api/acp/agents');
}

export function listConnections(): Promise<AcpConnectionSummary[]> {
  return apiFetch<AcpConnectionSummary[]>('/api/acp');
}

export function spawnAgent(agentId: string, projectId: string): Promise<AcpConnectionInfo> {
  return apiFetch<AcpConnectionInfo>('/api/acp/spawn', {
    method: 'POST',
    body: JSON.stringify({ agentId, projectId }),
  });
}

/** The agent's captured stderr ([INVENTED-11]) — often the only explanation. */
export function connectionStderr(connectionId: string): Promise<{ stderr: string }> {
  return apiFetch<{ stderr: string }>(
    `/api/acp/${encodeURIComponent(connectionId)}/stderr`,
  );
}

/** Kill the agent process group. Every session on it dies with it. */
export function killConnection(connectionId: string): Promise<void> {
  return apiFetch<void>(`/api/acp/${encodeURIComponent(connectionId)}`, {
    method: 'DELETE',
  });
}

export function listSessions(projectId: string): Promise<AcpSession[]> {
  return apiFetch<AcpSession[]>(
    `/api/projects/${encodeURIComponent(projectId)}/sessions`,
  );
}

export function createSession(projectId: string, connectionId: string): Promise<AcpSession> {
  return apiFetch<AcpSession>(
    `/api/projects/${encodeURIComponent(projectId)}/sessions`,
    { method: 'POST', body: JSON.stringify({ connectionId }) },
  );
}

/** Forget a session. The agent keeps running — see the route's docs. */
export function deleteSession(sessionId: string): Promise<void> {
  return apiFetch<void>(`/api/sessions/${encodeURIComponent(sessionId)}`, {
    method: 'DELETE',
  });
}

// ---- WebSocket payloads ----------------------------------------------------

/** Lifecycle of a session, mirroring the server's `SessionState`. */
export type AcpSessionState = 'idle' | 'prompting' | 'closed';

/** One permission choice, exactly as the agent offered it. */
export interface PermissionOptionView {
  /** The agent's own identifier — must round-trip verbatim. */
  optionId: string;
  name: string;
  kind: string;
}

/**
 * Server → client frames.
 *
 * Every frame from the event log carries `seq` and `sessionId`; `ready`, `pong`
 * and `error` do not come from the log. `error`'s lack of a `seq` is deliberate
 * (see the route's `send_error`): it answers the frame that caused it, so giving
 * it a sequence number would corrupt the replay cursor.
 */
export type AcpServerMessage =
  | { type: 'ready'; sessionId: string; connectionId: string; seq: number; state: AcpSessionState }
  | { type: 'pong'; ts: number | null }
  | { type: 'error'; sessionId?: string; message: string }
  | { type: 'message_chunk'; seq: number; sessionId: string; text: string }
  | { type: 'thought_chunk'; seq: number; sessionId: string; text: string }
  | { type: 'tool_call'; seq: number; sessionId: string; toolCall: ToolCallPayload }
  | { type: 'tool_call_update'; seq: number; sessionId: string; toolCall: ToolCallPatch }
  | { type: 'plan'; seq: number; sessionId: string; plan: PlanPayload }
  | { type: 'usage'; seq: number; sessionId: string; usage: Record<string, unknown> }
  | { type: 'mode'; seq: number; sessionId: string; mode: Record<string, unknown> }
  | {
      type: 'permission_request';
      seq: number;
      sessionId: string;
      requestId: string;
      toolCall: ToolCallPatch;
      options: PermissionOptionView[];
    }
  | {
      type: 'permission_resolved';
      seq: number;
      sessionId: string;
      requestId: string;
      outcome: string;
    }
  | { type: 'turn_complete'; seq: number; sessionId: string; stopReason: StopReason }
  | { type: 'session_state'; seq: number; sessionId: string; state: AcpSessionState }
  | { type: 'connection_closed'; seq: number; sessionId: string; reason: string }
  | { type: 'truncated'; seq: number; sessionId: string; fromSeq: number };

/**
 * All five are normal ends of a turn.
 *
 * `refusal` especially: the agent declined, which is an answer. Rendering it as
 * an error would misreport what happened.
 */
export type StopReason =
  | 'end_turn'
  | 'max_tokens'
  | 'max_turn_requests'
  | 'refusal'
  | 'cancelled';

/** A tool call as first announced. Only `toolCallId` is guaranteed present. */
export interface ToolCallPayload {
  toolCallId: string;
  title?: string;
  kind?: string;
  status?: string;
  content?: unknown[];
  locations?: unknown[];
  rawInput?: unknown;
  rawOutput?: unknown;
}

/**
 * A `tool_call_update` payload: a **sparse patch**.
 *
 * An absent field means *unchanged*, not *cleared* — so merging must skip
 * `undefined` rather than assign it, or the first status-only patch would wipe
 * the title the user is reading.
 */
export type ToolCallPatch = Partial<ToolCallPayload> & { toolCallId: string };

export interface PlanEntryPayload {
  content: string;
  priority?: string;
  status?: string;
}

/** A plan is a full snapshot: replace, never append. */
export interface PlanPayload {
  entries: PlanEntryPayload[];
}

/** Client → server frames (§3.2). */
export type AcpClientMessage =
  | { type: 'prompt'; text: string }
  | { type: 'cancel' }
  | { type: 'permission_response'; requestId: string; optionId?: string; cancelled?: boolean }
  | { type: 'ping'; ts: number };
