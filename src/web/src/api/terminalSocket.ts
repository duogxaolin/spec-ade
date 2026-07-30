// Terminal WebSocket client — SPEC-001 §3 protocol, plus reconnect.
//
// Wire format: output arrives as raw binary frames (feed straight to xterm.js);
// everything structured is JSON text in both directions. The client's one piece
// of real state is `cursor`: how many output bytes it has consumed. On reconnect
// it sends `?after_seq=<cursor>` and the server replays only the tail, so a page
// reload or a dropped socket doesn't lose (or duplicate) scrollback.

import { wsUrl } from './client';

/** Server → client control frames (SPEC-001 §3.2). */
export interface ReadyMessage {
  type: 'ready';
  id: string;
  pid: number | null;
  rows: number;
  cols: number;
  cwd: string;
  seq: number;
}

export interface ExitMessage {
  type: 'exit';
  code: number | null;
  signal: string | null;
}

export type ControlMessage =
  | ReadyMessage
  | ExitMessage
  | { type: 'cwd'; path: string }
  | { type: 'pong'; ts: number | null }
  | { type: 'truncated'; fromSeq: number }
  | { type: 'error'; message: string };

export type ConnectionState = 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface TerminalSocketHandlers {
  /** Raw PTY output — pass to `term.write()`. */
  onOutput(data: Uint8Array): void;
  onReady?(msg: ReadyMessage): void;
  onCwd?(path: string): void;
  onExit?(msg: ExitMessage): void;
  /** History was pruned before the client's cursor: the stream has a gap. */
  onTruncated?(fromSeq: number): void;
  onServerError?(message: string): void;
  onStateChange?(state: ConnectionState): void;
}

/** Injection seam so tests can drive a fake socket. */
export type SocketFactory = (url: string) => WebSocket;

export interface TerminalSocketOptions {
  /** Resume from this byte offset instead of replaying all history. */
  afterSeq?: number;
  /** Coalesce resize events during a window drag. */
  resizeDebounceMs?: number;
  /** Give up after this many consecutive failed connects. */
  maxReconnectAttempts?: number;
  socketFactory?: SocketFactory;
}

const DEFAULT_RESIZE_DEBOUNCE_MS = 50;
const DEFAULT_MAX_RECONNECT_ATTEMPTS = 8;
/** Backoff base; doubles per attempt up to `RECONNECT_MAX_MS`. */
const RECONNECT_BASE_MS = 250;
const RECONNECT_MAX_MS = 5_000;

export class TerminalSocket {
  private socket: WebSocket | null = null;
  /** Output bytes consumed so far — the replay cursor. */
  private cursor: number;
  private attempts = 0;
  /** Set by `dispose()` or a shell exit: stop trying to reconnect. */
  private finished = false;
  private state: ConnectionState = 'closed';

  private pendingSize: { rows: number; cols: number } | null = null;
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  private readonly resizeDebounceMs: number;
  private readonly maxReconnectAttempts: number;
  private readonly createSocket: SocketFactory;

  constructor(
    readonly terminalId: string,
    private readonly handlers: TerminalSocketHandlers,
    options: TerminalSocketOptions = {},
  ) {
    this.cursor = options.afterSeq ?? 0;
    this.resizeDebounceMs = options.resizeDebounceMs ?? DEFAULT_RESIZE_DEBOUNCE_MS;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? DEFAULT_MAX_RECONNECT_ATTEMPTS;
    this.createSocket = options.socketFactory ?? ((url) => new WebSocket(url));
  }

  /** Bytes consumed — what a reconnect resumes from. */
  get seq(): number {
    return this.cursor;
  }

  get connectionState(): ConnectionState {
    return this.state;
  }

  connect(): void {
    if (this.finished) return;

    this.setState(this.attempts === 0 ? 'connecting' : 'reconnecting');

    // `after_seq` omitted on a first connect with no cursor means "send all
    // history", which is what a freshly opened pane wants.
    const url = wsUrl(`/api/terminals/${encodeURIComponent(this.terminalId)}/ws`, {
      after_seq: this.cursor > 0 ? this.cursor : undefined,
    });

    const socket = this.createSocket(url);
    // Without this, browsers deliver binary frames as Blob and every write
    // becomes async — output would arrive out of order.
    socket.binaryType = 'arraybuffer';
    this.socket = socket;

    socket.onopen = () => {
      this.attempts = 0;
      this.setState('open');
      // Re-assert the size: the PTY may have been created at a default 24x80
      // while this pane is a different shape.
      if (this.pendingSize) this.flushResize();
    };

    socket.onmessage = (event: MessageEvent) => this.handleFrame(event.data);

    socket.onclose = () => {
      this.socket = null;
      if (this.finished) {
        this.setState('closed');
        return;
      }
      this.scheduleReconnect();
    };

    // A failed handshake fires `error` then `close`; reconnecting is handled in
    // `onclose` so it isn't attempted twice.
    socket.onerror = () => {};
  }

  /** Send keystrokes / pasted text. */
  sendInput(data: string): void {
    if (!data) return;
    // A lone surrogate (possible from some IMEs) is not valid UTF-8, and the
    // server rejects the frame. Encode those as bytes instead of dropping input.
    if (hasLoneSurrogate(data)) {
      this.sendBytes(new TextEncoder().encode(data));
      return;
    }
    this.sendJson({ type: 'input', data });
  }

  /** Send a whole line; the server appends the carriage return. */
  submit(data: string): void {
    this.sendJson({ type: 'submit', data });
  }

  /** Send raw bytes (binary paste, synthesized control sequences). */
  sendBytes(bytes: Uint8Array): void {
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.socket.send(bytes);
    }
  }

  /**
   * Request a new size. Debounced: `FitAddon` fires on every frame of a window
   * drag, and each resize is an `ioctl` plus a `SIGWINCH` to the shell.
   */
  resize(rows: number, cols: number): void {
    if (rows < 1 || cols < 1) return;
    this.pendingSize = { rows, cols };
    if (this.resizeTimer) clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => this.flushResize(), this.resizeDebounceMs);
  }

  ping(): void {
    this.sendJson({ type: 'ping', ts: Date.now() });
  }

  /** Close for good. The shell keeps running — only this view goes away. */
  dispose(): void {
    this.finished = true;
    if (this.resizeTimer) clearTimeout(this.resizeTimer);
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.resizeTimer = null;
    this.reconnectTimer = null;
    this.socket?.close();
    this.socket = null;
    this.setState('closed');
  }

  private flushResize(): void {
    this.resizeTimer = null;
    if (!this.pendingSize) return;
    this.sendJson({ type: 'resize', ...this.pendingSize });
  }

  private sendJson(payload: Record<string, unknown>): void {
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(payload));
    }
  }

  private handleFrame(data: unknown): void {
    if (typeof data === 'string') {
      this.handleControl(data);
      return;
    }
    if (data instanceof ArrayBuffer) {
      const bytes = new Uint8Array(data);
      // Advance the cursor by exactly what we received; this is what makes a
      // later reconnect resume in the right place.
      this.cursor += bytes.byteLength;
      this.handlers.onOutput(bytes);
    }
    // Blob shouldn't occur (binaryType is arraybuffer) and is ignored rather
    // than handled asynchronously, which would reorder output.
  }

  private handleControl(text: string): void {
    let msg: ControlMessage;
    try {
      msg = JSON.parse(text) as ControlMessage;
    } catch {
      this.handlers.onServerError?.(`unparseable control frame: ${text.slice(0, 120)}`);
      return;
    }

    switch (msg.type) {
      case 'ready':
        // Authoritative cursor after replay — trust the server over our own
        // byte count, since it knows what it actually sent.
        this.cursor = msg.seq;
        this.handlers.onReady?.(msg);
        break;
      case 'cwd':
        this.handlers.onCwd?.(msg.path);
        break;
      case 'exit':
        // Nothing left to reconnect to.
        this.finished = true;
        this.handlers.onExit?.(msg);
        break;
      case 'truncated':
        this.cursor = msg.fromSeq;
        this.handlers.onTruncated?.(msg.fromSeq);
        break;
      case 'error':
        this.handlers.onServerError?.(msg.message);
        break;
      case 'pong':
        break;
    }
  }

  private scheduleReconnect(): void {
    if (this.attempts >= this.maxReconnectAttempts) {
      this.finished = true;
      this.setState('closed');
      this.handlers.onServerError?.(
        `lost connection to terminal after ${this.attempts} attempts`,
      );
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
 * True if `text` contains an unpaired surrogate, which cannot be encoded as
 * valid UTF-8 and would make the server reject the whole frame.
 */
export function hasLoneSurrogate(text: string): boolean {
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = text.charCodeAt(i + 1);
      if (Number.isNaN(next) || next < 0xdc00 || next > 0xdfff) return true;
      i += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}
