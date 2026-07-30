// Unit tests for the ACP session WS client (SPEC-003 §7, "FE unit").
//
// The cursor and the terminal-vs-retryable close distinction are what matter
// here. A cursor that counts the wrong frames replays a conversation twice; a
// 1008 treated as a blip retries eight times against a server that will refuse
// every one of them.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { AcpSocket, stopReasonLabel } from './acpSocket';

/** Minimal `WebSocket` stand-in that records sends and lets tests push frames. */
class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = FakeSocket.CONNECTING;
  sent: string[] = [];

  onopen: (() => void) | null = null;
  onclose: ((event?: { code?: number; reason?: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  static instances: FakeSocket[] = [];
  static reset(): void {
    FakeSocket.instances = [];
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code: 1000 });
  }

  // --- test helpers ---
  open(): void {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  pushJson(value: unknown): void {
    this.onmessage?.({ data: JSON.stringify(value) });
  }

  /** Network-level drop: no code, the retryable case. */
  drop(): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code: 1006 });
  }

  /** The server refusing the session (§3.2). */
  reject(reason: string): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.({ code: 1008, reason });
  }

  jsonSent(): Array<Record<string, unknown>> {
    return this.sent.map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

function makeCaptured() {
  return {
    events: [] as Array<Record<string, unknown>>,
    ready: [] as Array<Record<string, unknown>>,
    errors: [] as string[],
    states: [] as string[],
    finished: [] as string[],
  };
}

function connect(options: { afterSeq?: number; maxReconnectAttempts?: number } = {}) {
  const captured = makeCaptured();
  const socket = new AcpSocket(
    'conn-1',
    'sess-1',
    {
      onEvent: (event) => captured.events.push(event as unknown as Record<string, unknown>),
      onReady: (msg) => captured.ready.push(msg as unknown as Record<string, unknown>),
      onServerError: (message) => captured.errors.push(message),
      onStateChange: (state) => captured.states.push(state),
      onFinished: (reason) => captured.finished.push(reason),
    },
    { ...options, socketFactory: (url) => new FakeSocket(url) as unknown as WebSocket },
  );
  socket.connect();
  return { socket, captured, fake: () => FakeSocket.instances.at(-1)! };
}

beforeEach(() => {
  FakeSocket.reset();
  vi.stubGlobal('WebSocket', FakeSocket);
  vi.stubGlobal('window', {
    location: { href: 'http://localhost:4123/' },
    history: { replaceState: () => {} },
  });
  const storage = new Map<string, string>();
  vi.stubGlobal('sessionStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
  });
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('url', () => {
  it('addresses a session on a connection', () => {
    const { fake } = connect();
    expect(fake().url).toContain('/api/acp/conn-1/ws');
    expect(fake().url).toContain('sessionId=sess-1');
    // Nothing seen yet means "replay everything", which is what a fresh pane
    // wants; sending after_seq=0 would be the same but noisier.
    expect(fake().url).not.toContain('after_seq');
  });

  it('resumes from the given cursor', () => {
    const { fake } = connect({ afterSeq: 17 });
    // snake_case is pinned by the server; `afterSeq` would be ignored and the
    // whole session replayed.
    expect(fake().url).toContain('after_seq=17');
  });
});

describe('event cursor', () => {
  it('advances by seq and resumes there after a drop', () => {
    const { socket, captured, fake } = connect();
    fake().open();

    fake().pushJson({ type: 'message_chunk', seq: 1, sessionId: 'sess-1', text: 'he' });
    fake().pushJson({ type: 'message_chunk', seq: 2, sessionId: 'sess-1', text: 'llo' });
    expect(socket.seq).toBe(2);
    expect(captured.events).toHaveLength(2);

    fake().drop();
    vi.advanceTimersByTime(300);
    expect(FakeSocket.instances[1]!.url).toContain('after_seq=2');
  });

  it('drops events at or below the cursor', () => {
    // Replay and broadcast overlap by design (§5.7), so the same seq can arrive
    // twice; folding it twice would duplicate the text on screen.
    const { captured, fake } = connect();
    fake().open();
    fake().pushJson({ type: 'message_chunk', seq: 3, sessionId: 'sess-1', text: 'a' });
    fake().pushJson({ type: 'message_chunk', seq: 3, sessionId: 'sess-1', text: 'a' });
    fake().pushJson({ type: 'message_chunk', seq: 2, sessionId: 'sess-1', text: 'stale' });

    expect(captured.events).toHaveLength(1);
  });

  it('takes the cursor from ready', () => {
    const { socket, captured, fake } = connect();
    fake().open();
    fake().pushJson({
      type: 'ready',
      sessionId: 'sess-1',
      connectionId: 'conn-1',
      seq: 9,
      state: 'prompting',
    });

    expect(socket.seq).toBe(9);
    expect(captured.ready[0]!.state).toBe('prompting');
    // `ready` is not part of the transcript.
    expect(captured.events).toHaveLength(0);
  });

  it('does not let truncated swallow the event it points at', () => {
    // `fromSeq` is where the stream *resumes*: adopting it as the cursor would
    // drop the first surviving event.
    const { socket, captured, fake } = connect({ afterSeq: 2 });
    fake().open();
    fake().pushJson({ type: 'truncated', seq: 40, sessionId: 'sess-1', fromSeq: 40 });
    fake().pushJson({ type: 'message_chunk', seq: 40, sessionId: 'sess-1', text: 'kept' });

    expect(captured.events.map((e) => e.type)).toEqual(['truncated', 'message_chunk']);
    expect(socket.seq).toBe(40);
  });

  it('does not count error frames, which carry no seq', () => {
    const { socket, captured, fake } = connect();
    fake().open();
    fake().pushJson({ type: 'message_chunk', seq: 5, sessionId: 'sess-1', text: 'x' });
    fake().pushJson({ type: 'error', sessionId: 'sess-1', message: 'session is busy' });

    expect(socket.seq).toBe(5);
    expect(captured.errors).toEqual(['session is busy']);
  });
});

describe('outbound frames', () => {
  it('sends prompt, cancel and permission answers', () => {
    const { socket, fake } = connect();
    fake().open();

    socket.prompt('list files');
    socket.cancel();
    socket.respondPermission('req-1', 'allow-once');
    socket.dismissPermission('req-2');

    expect(fake().jsonSent()).toEqual([
      { type: 'prompt', text: 'list files' },
      { type: 'cancel' },
      // The option id must round-trip verbatim: the agent only accepts its own.
      { type: 'permission_response', requestId: 'req-1', optionId: 'allow-once' },
      // A dismissal still has to reach the agent as an outcome.
      { type: 'permission_response', requestId: 'req-2', cancelled: true },
    ]);
  });

  it('drops sends made before the socket opens', () => {
    const { socket, fake } = connect();
    socket.prompt('too early');
    expect(fake().sent).toHaveLength(0);
  });
});

describe('terminal vs retryable close', () => {
  it('stops for good once the connection closes', () => {
    const { captured, fake } = connect();
    fake().open();
    fake().pushJson({
      type: 'connection_closed',
      seq: 7,
      sessionId: 'sess-1',
      reason: 'agent exited with code 1',
    });
    fake().drop();
    vi.advanceTimersByTime(60_000);

    // The agent process is gone; reconnecting would spin against a 404.
    expect(FakeSocket.instances).toHaveLength(1);
    expect(captured.finished).toEqual(['agent exited with code 1']);
    // Still delivered, so the transcript records why it ended.
    expect(captured.events.at(-1)!.type).toBe('connection_closed');
  });

  it('does not retry a 1008 rejection', () => {
    const { captured, fake } = connect();
    fake().reject('no session sess-1');
    vi.advanceTimersByTime(60_000);

    expect(FakeSocket.instances).toHaveLength(1);
    expect(captured.errors).toEqual(['no session sess-1']);
    expect(captured.finished).toEqual(['no session sess-1']);
  });

  it('backs off exponentially and gives up after the limit', () => {
    const { captured, fake } = connect({ maxReconnectAttempts: 2 });
    fake().open();

    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(250);
    expect(FakeSocket.instances).toHaveLength(2);

    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(500);
    expect(FakeSocket.instances).toHaveLength(3);

    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(60_000);
    expect(FakeSocket.instances).toHaveLength(3);
    expect(captured.finished.at(-1)).toContain('lost connection');
  });

  it('does not reconnect after dispose', () => {
    const { socket, fake } = connect();
    fake().open();
    socket.dispose();
    vi.advanceTimersByTime(60_000);
    expect(FakeSocket.instances).toHaveLength(1);
  });
});

describe('bad frames', () => {
  it('reports unparseable and binary frames', () => {
    const { captured, fake } = connect();
    fake().open();
    fake().onmessage?.({ data: 'not json' });
    fake().onmessage?.({ data: new ArrayBuffer(4) });

    expect(captured.errors[0]).toContain('unparseable');
    expect(captured.errors[1]).toContain('binary');
  });
});

describe('stopReasonLabel', () => {
  it('leaves the ordinary end of a turn unannotated', () => {
    expect(stopReasonLabel('end_turn')).toBe('');
  });

  it('describes the other reasons without calling them errors', () => {
    // A refusal is an answer, not a failure — the wording must not imply one.
    expect(stopReasonLabel('refusal')).toBe('the agent declined to continue');
    expect(stopReasonLabel('max_tokens')).toContain('token limit');
    expect(stopReasonLabel('max_turn_requests')).toContain('request limit');
    expect(stopReasonLabel('cancelled')).toBe('cancelled');
  });
});
