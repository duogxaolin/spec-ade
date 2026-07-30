// ACP store — which agents are running, which sessions are open, and what each
// one has said so far (SPEC-003 §5.8).
//
// The sockets live in a plain `Map` outside reactive state, the same rule
// `terminals.ts` follows for xterm: a `WebSocket` wrapped in a Vue proxy is a
// source of subtle breakage, and nothing renders from the socket object itself.
// What renders is the `SessionView` each socket folds its events into.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  createSession,
  deleteSession,
  killConnection,
  listAgents,
  listConnections,
  listSessions,
  spawnAgent,
  type AcpAgentEntry,
  type AcpConnectionSummary,
  type AcpSession,
} from '../api/acp';
import { AcpSocket, type ConnectionState } from '../api/acpSocket';
import { applyAcpEvent, createSessionView, type SessionView } from './acpSession';

export const useAcpStore = defineStore('acp', () => {
  const agents = ref<AcpAgentEntry[]>([]);
  const connections = ref<AcpConnectionSummary[]>([]);
  const sessions = ref<AcpSession[]>([]);
  const activeSessionId = ref<string | null>(null);
  /** One view per session id, folded from that session's event stream. */
  const views = ref<Record<string, SessionView>>({});
  /** Socket health per session, for the "reconnecting…" indicator. */
  const socketStates = ref<Record<string, ConnectionState>>({});
  const loading = ref(false);
  const error = ref<string | null>(null);

  // Not reactive: see the module comment.
  const sockets = new Map<string, AcpSocket>();

  const activeView = computed<SessionView | null>(() =>
    activeSessionId.value ? (views.value[activeSessionId.value] ?? null) : null,
  );

  /** Load the agent catalogue and any connections already running. */
  async function refresh(projectId?: string): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      // Agents and connections are independent reads; a session list needs a
      // project, so it only joins in when we have one.
      const [agentList, connectionList] = await Promise.all([listAgents(), listConnections()]);
      agents.value = agentList;
      connections.value = connectionList;
      if (projectId) {
        sessions.value = await listSessions(projectId);
      }
    } catch (err) {
      error.value = messageOf(err);
    } finally {
      loading.value = false;
    }
  }

  /**
   * Reuse this project's connection for `agentId`, or start one.
   *
   * One process per (agent, project) is the [INVENTED-1] rule: sessions are
   * cheap, agent processes are not.
   */
  async function ensureConnection(agentId: string, projectId: string): Promise<string | null> {
    const existing = connections.value.find(
      (c) => c.agentId === agentId && c.projectId === projectId,
    );
    if (existing) return existing.id;

    error.value = null;
    try {
      const info = await spawnAgent(agentId, projectId);
      // Refresh rather than synthesize the summary: `GET /api/acp` reports live
      // state and session counts that the spawn response does not carry.
      connections.value = await listConnections();
      return info.id;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  /** Create a session on `connectionId` and attach to its stream. */
  async function openSession(
    projectId: string,
    connectionId: string,
  ): Promise<AcpSession | null> {
    error.value = null;
    try {
      const session = await createSession(projectId, connectionId);
      sessions.value = [...sessions.value, session];
      attach(session);
      activeSessionId.value = session.id;
      return session;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  /**
   * Open a socket for a session, resuming from whatever this store already has.
   *
   * Idempotent: re-attaching an already-attached session is a no-op, so a
   * component remounting (tab switch, HMR) doesn't end up with two sockets
   * folding the same events twice.
   */
  function attach(session: AcpSession): void {
    if (sockets.has(session.id)) return;

    const view = views.value[session.id] ?? createSessionView();
    views.value = { ...views.value, [session.id]: view };

    const socket = new AcpSocket(
      session.connectionId,
      session.id,
      {
        onEvent: (event) => {
          const current = views.value[session.id];
          if (current) applyAcpEvent(current, event);
        },
        onReady: (msg) => {
          const current = views.value[session.id];
          if (!current) return;
          current.state = msg.state;
          // Trust the server's lifecycle over anything inferred from replay: a
          // client attaching mid-turn has to know the turn is still running.
          current.turnActive = msg.state === 'prompting';
          // The server's cursor is authoritative after replay.
          if (msg.seq > current.lastSeq) current.lastSeq = msg.seq;
        },
        onServerError: (message) => {
          error.value = message;
        },
        onStateChange: (state) => {
          socketStates.value = { ...socketStates.value, [session.id]: state };
        },
        onFinished: () => {
          // Keep the transcript on screen — it is the record of what happened —
          // but stop pretending the session is live.
          const current = views.value[session.id];
          if (current) current.turnActive = false;
        },
      },
      { afterSeq: view.lastSeq },
    );

    sockets.set(session.id, socket);
    socket.connect();
  }

  /** Close the socket, keeping the transcript and the server-side session. */
  function detach(sessionId: string): void {
    sockets.get(sessionId)?.dispose();
    sockets.delete(sessionId);
  }

  function select(sessionId: string): void {
    activeSessionId.value = sessionId;
  }

  /** Send a prompt. A busy session answers with an `error` frame (A15). */
  function prompt(sessionId: string, text: string): void {
    const trimmed = text.trim();
    if (!trimmed) return;
    const view = views.value[sessionId];
    // Optimistic: the server confirms with `session_state`, but the input has to
    // lock immediately or a fast second Enter opens a turn that will be refused.
    if (view) view.turnActive = true;
    sockets.get(sessionId)?.prompt(trimmed);
  }

  function cancel(sessionId: string): void {
    sockets.get(sessionId)?.cancel();
  }

  /** Answer the parked request with one of the agent's own option ids (A9). */
  function answerPermission(sessionId: string, optionId: string): void {
    const view = views.value[sessionId];
    const requestId = view?.permission?.requestId;
    if (!requestId) return;
    sockets.get(sessionId)?.respondPermission(requestId, optionId);
    // Not cleared here: `permission_resolved` clears it. Clearing on click would
    // hide the prompt even when the server rejects the option and leaves the
    // request parked for another try.
  }

  /** Dismiss without choosing — the agent still gets an outcome. */
  function dismissPermission(sessionId: string): void {
    const view = views.value[sessionId];
    const requestId = view?.permission?.requestId;
    if (!requestId) return;
    sockets.get(sessionId)?.dismissPermission(requestId);
  }

  /** Forget a session locally and on the server. The agent keeps running. */
  async function closeSession(sessionId: string): Promise<void> {
    detach(sessionId);
    error.value = null;
    try {
      await deleteSession(sessionId);
    } catch (err) {
      // Report, then still drop it: a 404 means it is already gone, and leaving
      // a dead tab on screen is worse than a stale error.
      error.value = messageOf(err);
    }
    sessions.value = sessions.value.filter((s) => s.id !== sessionId);
    const { [sessionId]: _dropped, ...rest } = views.value;
    views.value = rest;
    if (activeSessionId.value === sessionId) {
      activeSessionId.value = sessions.value[0]?.id ?? null;
    }
  }

  /** Kill an agent process. Every session on it dies with it. */
  async function kill(connectionId: string): Promise<void> {
    error.value = null;
    try {
      await killConnection(connectionId);
    } catch (err) {
      error.value = messageOf(err);
    }
    for (const session of sessions.value.filter((s) => s.connectionId === connectionId)) {
      detach(session.id);
    }
    connections.value = connections.value.filter((c) => c.id !== connectionId);
  }

  /** Drop every socket — for a pane teardown or a full page navigation. */
  function disposeAll(): void {
    for (const id of [...sockets.keys()]) detach(id);
  }

  return {
    agents,
    connections,
    sessions,
    activeSessionId,
    views,
    socketStates,
    loading,
    error,
    activeView,
    refresh,
    ensureConnection,
    openSession,
    attach,
    detach,
    select,
    prompt,
    cancel,
    answerPermission,
    dismissPermission,
    closeSession,
    kill,
    disposeAll,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
