// Unit tests for the terminal WS client (SPEC-001 §3, test matrix "FE unit").
//
// The reconnect cursor is the piece worth testing hard: get it wrong and a page
// reload either loses output or duplicates it, which is invisible until a user
// is halfway through reading a build log.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { TerminalSocket, hasLoneSurrogate } from './terminalSocket';

/** Minimal `WebSocket` stand-in that records sends and lets tests push frames. */
class FakeSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = FakeSocket.CONNECTING;
  binaryType = 'blob';
  sent: Array<string | Uint8Array> = [];

  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  static instances: FakeSocket[] = [];
  static reset(): void {
    FakeSocket.instances = [];
  }

  send(data: string | Uint8Array): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.();
  }

  // --- test helpers ---
  open(): void {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  pushJson(value: unknown): void {
    this.onmessage?.({ data: JSON.stringify(value) });
  }

  pushBytes(bytes: Uint8Array): void {
    // Browsers hand over an ArrayBuffer when binaryType is 'arraybuffer'.
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }

  drop(): void {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.();
  }

  /** Everything sent as JSON, parsed. */
  jsonSent(): Array<Record<string, unknown>> {
    return this.sent
      .filter((s): s is string => typeof s === 'string')
      .map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

/** Collect what a socket handed to the UI layer. */
function makeHandlers() {
  return {
    output: [] as Uint8Array[],
    ready: [] as unknown[],
    cwd: [] as string[],
    exits: [] as unknown[],
    truncations: [] as number[],
    errors: [] as string[],
    states: [] as string[],
  };
}

function connect(
  options: { afterSeq?: number; resizeDebounceMs?: number; maxReconnectAttempts?: number } = {},
) {
  const captured = makeHandlers();
  const socket = new TerminalSocket(
    'term-1',
    {
      onOutput: (data) => captured.output.push(data),
      onReady: (msg) => captured.ready.push(msg),
      onCwd: (path) => captured.cwd.push(path),
      onExit: (msg) => captured.exits.push(msg),
      onTruncated: (seq) => captured.truncations.push(seq),
      onServerError: (msg) => captured.errors.push(msg),
      onStateChange: (state) => captured.states.push(state),
    },
    {
      ...options,
      socketFactory: (url) => new FakeSocket(url) as unknown as WebSocket,
    },
  );
  socket.connect();
  return { socket, captured, fake: () => FakeSocket.instances.at(-1)! };
}

beforeEach(() => {
  FakeSocket.reset();
  // The client compares `readyState` against the global `WebSocket` constants.
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

describe('connection lifecycle', () => {
  it('requests binary frames as ArrayBuffer', () => {
    const { fake } = connect();
    // Blob delivery would make every write async and reorder output.
    expect(fake().binaryType).toBe('arraybuffer');
  });

  it('omits after_seq on a fresh connect and includes it when resuming', () => {
    const fresh = connect();
    expect(fresh.fake().url).not.toContain('after_seq');
    expect(fresh.fake().url).toContain('/api/terminals/term-1/ws');

    FakeSocket.reset();
    const resumed = connect({ afterSeq: 4096 });
    expect(resumed.fake().url).toContain('after_seq=4096');
  });

  it('reports state transitions', () => {
    const { captured, fake } = connect();
    expect(captured.states).toEqual(['connecting']);
    fake().open();
    expect(captured.states).toEqual(['connecting', 'open']);
  });
});

describe('output cursor', () => {
  it('advances by the exact byte length received', () => {
    const { socket, captured, fake } = connect();
    fake().open();

    fake().pushBytes(new Uint8Array([1, 2, 3]));
    fake().pushBytes(new Uint8Array([4, 5]));

    expect(socket.seq).toBe(5);
    expect(captured.output.map((c) => Array.from(c))).toEqual([[1, 2, 3], [4, 5]]);
  });

  it('trusts the server cursor reported by ready', () => {
    // After replay the server knows exactly what it sent; our own count of
    // pre-ready frames must not double-count it.
    const { socket, fake } = connect();
    fake().open();
    fake().pushBytes(new Uint8Array([1, 2, 3]));
    fake().pushJson({ type: 'ready', id: 'term-1', pid: 1, rows: 24, cols: 80, cwd: '/', seq: 3 });

    expect(socket.seq).toBe(3);
  });

  it('resumes from the cursor after a dropped socket', () => {
    const { socket, fake } = connect();
    fake().open();
    fake().pushBytes(new Uint8Array([1, 2, 3, 4]));
    expect(socket.seq).toBe(4);

    fake().drop();
    vi.advanceTimersByTime(300);

    // The reconnect asks for exactly what it hasn't seen.
    expect(FakeSocket.instances).toHaveLength(2);
    expect(FakeSocket.instances[1].url).toContain('after_seq=4');
  });

  it('rewinds the cursor when the server reports truncation', () => {
    const { socket, captured, fake } = connect({ afterSeq: 10 });
    fake().open();
    fake().pushJson({ type: 'truncated', fromSeq: 500 });

    expect(captured.truncations).toEqual([500]);
    // History before 500 is gone; the client must not keep asking for it.
    expect(socket.seq).toBe(500);
  });
});

describe('outbound frames', () => {
  it('sends input as a JSON frame', () => {
    const { socket, fake } = connect();
    fake().open();
    socket.sendInput('ls -la\r');

    expect(fake().jsonSent()).toEqual([{ type: 'input', data: 'ls -la\r' }]);
  });

  it('drops empty input instead of sending a noop frame', () => {
    const { socket, fake } = connect();
    fake().open();
    socket.sendInput('');
    expect(fake().sent).toHaveLength(0);
  });

  it('sends submit without a newline (the server appends CR)', () => {
    const { socket, fake } = connect();
    fake().open();
    socket.submit('echo hi');
    expect(fake().jsonSent()).toEqual([{ type: 'submit', data: 'echo hi' }]);
  });

  it('encodes lone surrogates as bytes rather than losing the input', () => {
    const { socket, fake } = connect();
    fake().open();
    // A lone high surrogate can't be valid UTF-8; sent as JSON the server would
    // reject the whole frame.
    socket.sendInput('\ud800');
    expect(fake().jsonSent()).toHaveLength(0);
    expect(fake().sent[0]).toBeInstanceOf(Uint8Array);
  });

  it('buffers sends made before the socket opens', () => {
    // readyState is CONNECTING: sending now would throw in a real browser.
    const { socket, fake } = connect();
    socket.sendInput('too early');
    expect(fake().sent).toHaveLength(0);
  });
});

describe('resize', () => {
  it('debounces a burst into one frame carrying the final size', () => {
    const { socket, fake } = connect({ resizeDebounceMs: 50 });
    fake().open();

    socket.resize(24, 80);
    socket.resize(30, 100);
    socket.resize(40, 120);
    expect(fake().jsonSent()).toHaveLength(0);

    vi.advanceTimersByTime(50);
    expect(fake().jsonSent()).toEqual([{ type: 'resize', rows: 40, cols: 120 }]);
  });

  it('ignores a degenerate size', () => {
    // A hidden pane measures 0; a zero winsize makes curses apps misbehave.
    const { socket, fake } = connect();
    fake().open();
    socket.resize(0, 0);
    vi.advanceTimersByTime(100);
    expect(fake().jsonSent()).toHaveLength(0);
  });

  it('re-asserts the size on reconnect', () => {
    const { socket, fake } = connect({ resizeDebounceMs: 10 });
    fake().open();
    socket.resize(40, 120);
    vi.advanceTimersByTime(10);

    fake().drop();
    vi.advanceTimersByTime(300);
    const reconnected = FakeSocket.instances[1];
    reconnected.open();

    // The new socket learns the pane's real size without waiting for the user
    // to resize the window again.
    expect(reconnected.jsonSent()).toEqual([{ type: 'resize', rows: 40, cols: 120 }]);
  });
});

describe('control messages', () => {
  it('surfaces ready, cwd and exit', () => {
    const { captured, fake } = connect();
    fake().open();

    fake().pushJson({ type: 'ready', id: 'term-1', pid: 42, rows: 24, cols: 80, cwd: '/x', seq: 0 });
    fake().pushJson({ type: 'cwd', path: '/tmp' });
    fake().pushJson({ type: 'exit', code: 0, signal: null });

    expect(captured.ready).toHaveLength(1);
    expect(captured.cwd).toEqual(['/tmp']);
    expect(captured.exits).toEqual([{ type: 'exit', code: 0, signal: null }]);
  });

  it('stops reconnecting once the shell has exited', () => {
    const { fake } = connect();
    fake().open();
    fake().pushJson({ type: 'exit', code: 0, signal: null });
    fake().drop();
    vi.advanceTimersByTime(10_000);

    // There is nothing to reconnect to — retrying would spin forever on 404s.
    expect(FakeSocket.instances).toHaveLength(1);
  });

  it('reports server errors and unparseable frames', () => {
    const { captured, fake } = connect();
    fake().open();
    fake().pushJson({ type: 'error', message: 'resize failed' });
    fake().onmessage?.({ data: 'not json at all' });

    expect(captured.errors[0]).toBe('resize failed');
    expect(captured.errors[1]).toContain('unparseable');
  });
});

describe('reconnect backoff', () => {
  it('backs off exponentially and gives up after the limit', () => {
    const { captured, fake } = connect({ maxReconnectAttempts: 3 });
    fake().open();

    // Each drop schedules the next attempt with a longer delay.
    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(250);
    expect(FakeSocket.instances).toHaveLength(2);

    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(500);
    expect(FakeSocket.instances).toHaveLength(3);

    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(1_000);
    expect(FakeSocket.instances).toHaveLength(4);

    // Fourth drop exceeds the limit: report instead of retrying forever.
    FakeSocket.instances.at(-1)!.drop();
    vi.advanceTimersByTime(60_000);
    expect(FakeSocket.instances).toHaveLength(4);
    expect(captured.errors.at(-1)).toContain('lost connection');
  });

  it('does not reconnect after dispose', () => {
    // Closing a pane must not leave a socket resurrecting itself.
    const { socket, fake } = connect();
    fake().open();
    socket.dispose();
    vi.advanceTimersByTime(10_000);
    expect(FakeSocket.instances).toHaveLength(1);
  });
});

describe('hasLoneSurrogate', () => {
  it('accepts well-formed text including astral characters', () => {
    for (const text of ['', 'plain ascii', 'áéíóú 日本語', '😀 emoji', 'a😀b']) {
      expect(hasLoneSurrogate(text)).toBe(false);
    }
  });

  it('detects unpaired surrogates', () => {
    expect(hasLoneSurrogate('\ud800')).toBe(true); // high, nothing after
    expect(hasLoneSurrogate('\udc00')).toBe(true); // low, nothing before
    expect(hasLoneSurrogate('a\ud800b')).toBe(true); // high followed by BMP
    expect(hasLoneSurrogate('\ud800\ud800')).toBe(true); // two highs
  });
});
