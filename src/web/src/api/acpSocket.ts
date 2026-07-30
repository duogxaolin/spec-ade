// ACP session WebSocket client — SPEC-003 §3.2, plus reconnect.
//
// Same shape as `terminalSocket.ts` (backoff, `after_seq` cursor, injectable
// factory) with three differences that come from the protocol, not from taste:
//
// 1. Every frame is JSON text. There is no binary path, so the cursor advances
//    by event `seq` rather than by byte count.
// 2. A socket addresses a *session on a connection*, so both ids go in the URL:
//    `/api/acp/{connectionId}/ws?sessionId=…`. A session belonging to another
//    connection is refused with close code 1008 — reconnecting would just be
//    refused again, so that is terminal.
// 3. `connection_closed` is this protocol's `exit`: the agent process is gone
//    and nothing further will ever arrive for the session.

import type { AcpClientMessage, AcpServerMessage, AcpSessionState, StopReason } from './acp';
import { wsUrl } from './client';

export type ConnectionState = 'connecting' | 'open' | 'reconnecting' | 'closed';

/** Injection seam so tests can drive a fake socket. */
export type SocketFactory = (url: string) => WebSocket;

export interface AcpSocketHandlers {
  /**
   * Every sequenced event, already deduped against the cursor.
   *
   * One callback rather than one per variant: the store folds them in a single
   * `switch`, and a per-variant surface would have to grow every time the ACP
   * spec adds an update type.
   */
  onEvent(event: AcpServerMessage & { seq: number }): void;
  /** Sent after replay finishes; `seq` is the authoritative cursor. */
  onReady?(msg: { sessionId: string; connectionId: string; seq: number; state: AcpSessionState }): void;
  /** A server-side complaint about something we sent. Carries no `seq`. */
  onServerError?(message: string): void;
  onStateChange?(state: ConnectionState): void;
  /** The stream ended for good: agent gone, or reconnects exhausted. */
  onFinished?(reason: string): void;
}

export interface AcpSocketOptions {
  /** Resume after this event `seq` instead of replaying the whole session. */
  afterSeq?: number;
  maxReconnectAttempts?: number;
  socketFactory?: SocketFactory;
}

const DEFAULT_MAX_RECONNECT_ATTEMPTS = 8;
const RECONNECT_BASE_MS = 250;
const RECONNECT_MAX_MS = 5_000;
/** Close code the server uses for a bad `sessionId` (§3.2). */
const CLOSE_POLICY = 1008;

export class AcpSocket {
  private socket: WebSocket | null = null;
  /** Highest event `seq` delivered — what a reconnect resumes after. */
  private cursor: number;
  private attempts = 0;
  /** Set by `dispose()`, a closed connection, or a 1008: stop reconnecting. */
  private finished = false;
  private state: ConnectionState = 'closed';
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  private readonly maxReconnectAttempts: number;
  private readonly createSocket: SocketFactory;

  constructor(
    readonly connectionId: string,
    readonly sessionId: string,
    private readonly handlers: AcpSocketHandlers,
    options: AcpSocketOptions = {},
  ) {
    this.cursor = options.afterSeq ?? 0;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? DEFAULT_MAX_RECONNECT_ATTEMPTS;
    this.createSocket = options.socketFactory ?? ((url) => new WebSocket(url));
  }

  /** The replay cursor — pass as `afterSeq` to resume a fresh socket here. */
  get seq(): number {
    return this.cursor;
  }

  get connectionState(): ConnectionState {
    return this.state;
  }

  connect(): void {
    if (this.finished) return;

    this.setState(this.attempts === 0 ? 'connecting' : 'reconnecting');

    // `after_seq` is snake_case while `sessionId` is camelCase: the server pins
    // both spellings (it matches SPEC-001's terminal socket for the cursor), and
    // renaming either one silently replays the entire session.
    const url = wsUrl(`/api/acp/${encodeURIComponent(this.connectionId)}/ws`, {
      sessionId: this.sessionId,
      after_seq: this.cursor > 0 ? this.cursor : undefined,
    });

    const socket = this.createSocket(url);
    this.socket = socket;

    socket.onopen = () => {
      this.attempts = 0;
      this.setState('open');
    };

    socket.onmessage = (event: MessageEvent) => this.handleFrame(event.data);

    socket.onclose = (event: CloseEvent) => {
      this.socket = null;
      // 1008 means the server rejected this session/connection pair. Retrying
      // would be rejected identically, so treat it as an error, not a blip.
      if (!this.finished && event?.code === CLOSE_POLICY) {
        this.finished = true;
        this.setState('closed');
        this.handlers.onServerError?.(event.reason || 'session rejected by server');
        this.handlers.onFinished?.(event.reason || 'session rejected by server');
        return;
      }
      if (this.finished) {
        this.setState('closed');
        return;
      }
      this.scheduleReconnect();
    };

    socket.onerror = () => {};
  }

  /** Open a turn. A second one while a turn runs comes back as an `error`. */
  prompt(text: string): void {
    this.send({ type: 'prompt', text });
  }

  cancel(): void {
    this.send({ type: 'cancel' });
  }

  /**
   * Answer a parked permission request.
   *
   * `optionId` must be one the agent offered verbatim; anything else comes back
   * as an `error` and the request stays parked (A10).
   */
  respondPermission(requestId: string, optionId: string): void {
    this.send({ type: 'permission_response', requestId, optionId });
  }

  /** Dismiss a permission request without choosing — still reaches the agent. */
  dismissPermission(requestId: string): void {
    this.send({ type: 'permission_response', requestId, cancelled: true });
  }

  ping(): void {
    this.send({ type: 'ping', ts: Date.now() });
  }

  /** Close this view. The agent keeps running and the session survives (A21). */
  dispose(): void {
    this.finished = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    this.socket?.close();
    this.socket = null;
    this.setState('closed');
  }

  private send(payload: AcpClientMessage): void {
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(payload));
    }
  }

  private handleFrame(data: unknown): void {
    if (typeof data !== 'string') {
      // Everything on this socket is JSON text; a binary frame is a server bug.
      this.handlers.onServerError?.('unexpected binary frame');
      return;
    }

    let msg: AcpServerMessage;
    try {
      msg = JSON.parse(data) as AcpServerMessage;
    } catch {
      this.handlers.onServerError?.(`unparseable frame: ${data.slice(0, 120)}`);
      return;
    }

    switch (msg.type) {
      case 'ready':
        // Authoritative after replay: the server knows what it actually sent.
        this.cursor = msg.seq;
        this.handlers.onReady?.(msg);
        return;
      case 'pong':
        return;
      case 'error':
        // No `seq` by design — it answers a frame we sent, so counting it would
        // corrupt the cursor.
        this.handlers.onServerError?.(msg.message);
        return;
      case 'truncated':
        // `fromSeq` is where the stream *resumes*, i.e. the seq of the next real
        // event, so it must not become the cursor: doing that would skip that
        // event. Left alone, the following events advance it normally.
        this.deliver(msg);
        return;
      case 'connection_closed':
        this.deliver(msg);
        this.finished = true;
        this.handlers.onFinished?.(msg.reason);
        return;
      default:
        this.deliver(msg);
    }
  }

  /** Deliver a sequenced event, dropping anything already seen. */
  private deliver(msg: AcpServerMessage & { seq: number }): void {
    // The server dedupes per socket, but replay and broadcast overlap by design
    // and a reconnect re-requests from the cursor, so the client filters too.
    // `truncated` is exempt: it reports a gap *at* an unseen seq.
    if (msg.type !== 'truncated' && msg.seq <= this.cursor) return;
    if (msg.type !== 'truncated') this.cursor = msg.seq;
    this.handlers.onEvent(msg);
  }

  private scheduleReconnect(): void {
    if (this.attempts >= this.maxReconnectAttempts) {
      this.finished = true;
      this.setState('closed');
      const reason = `lost connection to agent after ${this.attempts} attempts`;
      this.handlers.onServerError?.(reason);
      this.handlers.onFinished?.(reason);
      return;
    }
    const delay = Math.min(RECONNECT_BASE_MS * 2 ** this.attempts, RECONNECT_MAX_MS);
    this.attempts += 1;
    this.setState('reconnecting');
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private setState(state: ConnectionState): void {
    if (this.state === state) return;
    this.state = state;
    this.handlers.onStateChange?.(state);
  }
}

/**
 * How a turn ended, in words.
 *
 * All five reasons are normal ends of a turn, so none of these is phrased as an
 * error. `end_turn` gets an empty string: the ordinary case needs no annotation,
 * and labelling it would add a line under every single reply.
 */
export function stopReasonLabel(reason: StopReason): string {
  switch (reason) {
    case 'end_turn':
      return '';
    case 'max_tokens':
      return 'stopped at the token limit';
    case 'max_turn_requests':
      return 'stopped at the request limit for this turn';
    case 'refusal':
      return 'the agent declined to continue';
    case 'cancelled':
      return 'cancelled';
    default:
      // An unknown reason is still an end of turn, not a failure.
      return reason;
  }
}
