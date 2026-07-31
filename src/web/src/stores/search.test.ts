// Unit tests for the search store (D41–D43).
//
// All three rules are about *timing*, which no integration test can arrange: the
// order of close-vs-open, five keystrokes inside one debounce window, and a late
// frame from a stream that was already closed. So `searchEventSource` is mocked
// and the frames are delivered by hand.

import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { DEBOUNCE_MS, useSearchStore } from './search';

const { searchEventSource } = vi.hoisted(() => ({ searchEventSource: vi.fn() }));

vi.mock('../api/search', async () => {
  const actual = await vi.importActual<typeof import('../api/search')>('../api/search');
  return { ...actual, searchEventSource };
});

const PROJECT = 'p1';

/** Only what the store touches. jsdom has no `EventSource`. */
class FakeEventSource {
  onerror: (() => void) | null = null;
  closed = false;
  /** Order of construction, so "closed before the next was created" is assertable. */
  static log: string[] = [];
  readonly id: number;
  private listeners = new Map<string, ((event: unknown) => void)[]>();

  constructor(id: number) {
    this.id = id;
    FakeEventSource.log.push(`open:${id}`);
  }

  addEventListener(type: string, fn: (event: unknown) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
  }

  close(): void {
    this.closed = true;
    FakeEventSource.log.push(`close:${this.id}`);
  }

  emit(type: string, data: unknown): void {
    const payload = typeof data === 'string' ? data : JSON.stringify(data);
    for (const fn of this.listeners.get(type) ?? []) fn({ data: payload });
  }

  fail(): void {
    this.onerror?.();
  }
}

function match(path: string, line: number, text = 'needle') {
  return { path, line, text, ranges: [[0, 6]] };
}

function done(overrides: Record<string, unknown> = {}) {
  return {
    matches: 0,
    files: 0,
    filesScanned: 3,
    truncated: false,
    elapsedMs: 12,
    ...overrides,
  };
}

describe('search store', () => {
  let streams: FakeEventSource[];

  beforeEach(() => {
    setActivePinia(createPinia());
    vi.useFakeTimers();
    streams = [];
    FakeEventSource.log = [];
    searchEventSource.mockReset().mockImplementation(() => {
      const es = new FakeEventSource(streams.length);
      streams.push(es);
      return es;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('debounces five keystrokes inside 100ms into one stream (D42)', () => {
    const store = useSearchStore();
    for (const text of ['n', 'ne', 'nee', 'need', 'needl']) {
      store.search(PROJECT, text);
      vi.advanceTimersByTime(20);
    }
    // 100ms of typing, still inside the window: nothing has been sent.
    expect(searchEventSource).not.toHaveBeenCalled();

    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(searchEventSource).toHaveBeenCalledTimes(1);
    // And with the *last* query, not the first.
    expect(searchEventSource.mock.calls[0][1]).toMatchObject({ query: 'needl' });
  });

  it('sends one stream per query when typing is slower than the debounce', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'a');
    vi.advanceTimersByTime(DEBOUNCE_MS + 1);
    store.search(PROJECT, 'ab');
    vi.advanceTimersByTime(DEBOUNCE_MS + 1);
    expect(searchEventSource).toHaveBeenCalledTimes(2);
  });

  it('closes the old stream before opening the new one (D41)', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'first');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    store.search(PROJECT, 'second');
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(streams).toHaveLength(2);
    expect(streams[0].closed).toBe(true);
    expect(streams[1].closed).toBe(false);
    // The *ordering* is the assertion: close must precede the next open, because
    // closing is what cancels the walk server-side (§5.4). Opening first would
    // leave two walks filling the same result list.
    expect(FakeEventSource.log).toEqual(['open:0', 'close:0', 'open:1']);
  });

  it('ignores a late frame from a closed stream', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    const first = streams[0];

    store.search(PROJECT, 'other');
    vi.advanceTimersByTime(DEBOUNCE_MS);

    // `close()` does not retract frames already dispatched.
    first.emit('match', match('stale.ts', 1));
    expect(store.groups).toEqual([]);
    expect(store.matchCount).toBe(0);
  });

  it('groups streamed matches by file, keeping first-appearance order', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    const es = streams[0];

    es.emit('match', match('z.ts', 1));
    es.emit('match', match('a.ts', 4));
    es.emit('match', match('z.ts', 9));

    expect(store.groups.map((g) => g.path)).toEqual(['z.ts', 'a.ts']);
    expect(store.groups[0].matches.map((m) => m.line)).toEqual([1, 9]);
    expect(store.matchCount).toBe(3);
  });

  it('stops running on done and surfaces truncated (D43)', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(store.running).toBe(true);

    const es = streams[0];
    es.emit('match', match('a.ts', 1));
    es.emit('done', done({ matches: 2000, files: 12, truncated: true }));

    expect(store.running).toBe(false);
    expect(store.truncated).toBe(true);
    expect(store.matchCount).toBe(2000);
    expect(store.fileCount).toBe(12);
    expect(store.elapsedMs).toBe(12);
    // Nothing more is coming, so the socket is closed rather than left open.
    expect(es.closed).toBe(true);
  });

  it('reports done without truncation as a complete result', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    streams[0].emit('done', done({ matches: 3, files: 1 }));
    expect(store.truncated).toBe(false);
    expect(store.running).toBe(false);
  });

  it('records per-file errors without ending the search', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    const es = streams[0];

    es.emit('error', { path: 'locked.txt', detail: 'permission denied' });
    expect(store.errors).toEqual([{ path: 'locked.txt', detail: 'permission denied' }]);
    expect(store.running).toBe(true);
  });

  it('treats a dataless transport error as the end of the stream, not a file error', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    const es = streams[0];

    es.fail();
    expect(store.errors).toEqual([]);
    // `EventSource` would otherwise retry the entire search forever.
    expect(store.running).toBe(false);
    expect(es.closed).toBe(true);
  });

  it('empty is true only after a search that found nothing', () => {
    const store = useSearchStore();
    expect(store.empty).toBe(false); // nothing has run yet

    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(store.empty).toBe(false); // still running

    streams[0].emit('done', done());
    expect(store.empty).toBe(true);
  });

  it('an empty query clears results and opens no stream', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    streams[0].emit('match', match('a.ts', 1));
    expect(store.groups).toHaveLength(1);

    store.search(PROJECT, '   ');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(store.groups).toEqual([]);
    expect(store.running).toBe(false);
    expect(searchEventSource).toHaveBeenCalledTimes(1);
  });

  it('passes the toggles through and re-runs when one changes', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'a(');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(searchEventSource.mock.calls[0][1]).toMatchObject({ regex: false });

    store.setOptions(PROJECT, { regex: true, globs: ['*.rs', '!*.ts'] });
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(searchEventSource).toHaveBeenCalledTimes(2);
    expect(searchEventSource.mock.calls[1][1]).toMatchObject({
      query: 'a(',
      regex: true,
      globs: ['*.rs', '!*.ts'],
    });
  });

  it('does not run on a toggle change while the query is empty', () => {
    const store = useSearchStore();
    store.setOptions(PROJECT, { regex: true });
    vi.advanceTimersByTime(DEBOUNCE_MS * 2);
    expect(searchEventSource).not.toHaveBeenCalled();
  });

  it('stop cancels a pending debounce so no stream opens', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    store.stop();
    vi.advanceTimersByTime(DEBOUNCE_MS * 2);
    expect(searchEventSource).not.toHaveBeenCalled();
    expect(store.running).toBe(false);
  });

  it('stop closes the stream but keeps what was already found', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    streams[0].emit('match', match('a.ts', 1));

    store.stop();
    expect(streams[0].closed).toBe(true);
    expect(store.running).toBe(false);
    expect(store.groups).toHaveLength(1);
  });

  it('reset clears the query and the results', () => {
    const store = useSearchStore();
    store.search(PROJECT, 'needle');
    vi.advanceTimersByTime(DEBOUNCE_MS);
    streams[0].emit('match', match('a.ts', 1));

    store.reset();
    expect(store.query).toBe('');
    expect(store.groups).toEqual([]);
    expect(store.options).toEqual({
      regex: false,
      case: false,
      word: false,
      globs: [],
      path: null,
    });
  });
});
