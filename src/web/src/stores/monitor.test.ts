// Unit tests for the monitor store (D44, D45).
//
// Both rules are about state a running server cannot be made to produce on
// demand: 61 consecutive samples (3 minutes of wall clock) and three consecutive
// stream failures. So `systemEventSource`/`fetchMetrics` are mocked and the
// frames and failures are delivered by hand.

import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { MAX_STREAM_ERRORS, POLL_INTERVAL_MS, useMonitorStore } from './monitor';
import { HISTORY_LIMIT } from '../monitor/sparkline';
import type { Metrics } from '../api/system';

const { systemEventSource, fetchMetrics, killProcess } = vi.hoisted(() => ({
  systemEventSource: vi.fn(),
  fetchMetrics: vi.fn(),
  killProcess: vi.fn(),
}));

vi.mock('../api/system', async () => {
  const actual = await vi.importActual<typeof import('../api/system')>('../api/system');
  return { ...actual, systemEventSource, fetchMetrics, killProcess };
});

/** Only what the store touches. jsdom has no `EventSource`. */
class FakeEventSource {
  onerror: (() => void) | null = null;
  onopen: (() => void) | null = null;
  closed = false;
  private listeners = new Map<string, ((event: unknown) => void)[]>();

  addEventListener(type: string, fn: (event: unknown) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
  }

  close(): void {
    this.closed = true;
  }

  emit(type: string, data: unknown): void {
    const payload = typeof data === 'string' ? data : JSON.stringify(data);
    for (const fn of this.listeners.get(type) ?? []) fn({ data: payload });
  }

  fail(): void {
    this.onerror?.();
  }

  open(): void {
    this.onopen?.();
  }
}

function sample(overrides: Partial<Metrics> = {}): Metrics {
  return {
    timestampMs: 1,
    cpu: { usage: 10, coreCount: 8, perCore: [] },
    memory: { total: 1000, used: 250, swapTotal: 0, swapUsed: 0 },
    host: { name: 'h', os: 'macOS', uptimeSec: 60, loadAvg: [0, 0, 0] },
    gpu: null,
    processes: [],
    processCount: 0,
    truncated: false,
    ...overrides,
  };
}

function proc(pid: number, name: string, cmd = name, cpu = 1) {
  return {
    pid,
    parentPid: 1,
    name,
    cmd,
    cpu,
    memory: 1024,
    status: 'Run',
    runTimeSec: 5,
    user: 'me',
  };
}

describe('monitor store', () => {
  let streams: FakeEventSource[];

  beforeEach(() => {
    setActivePinia(createPinia());
    vi.useFakeTimers();
    streams = [];
    systemEventSource.mockReset().mockImplementation(() => {
      const es = new FakeEventSource();
      streams.push(es);
      return es;
    });
    fetchMetrics.mockReset().mockResolvedValue(sample());
    killProcess.mockReset().mockResolvedValue({ pid: 1, signal: 'term', delivered: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('keeps at most 60 history points, dropping the oldest (D44)', () => {
    const store = useMonitorStore();
    store.startWatch();
    const es = streams[0];

    for (let i = 0; i < HISTORY_LIMIT + 5; i += 1) {
      es.emit('metrics', sample({ cpu: { usage: i, coreCount: 8, perCore: [] } }));
    }

    expect(store.cpuHistory).toHaveLength(HISTORY_LIMIT);
    // The window slid: the first five samples are gone, the newest is last.
    expect(store.cpuHistory[0]).toBe(5);
    expect(store.cpuHistory[HISTORY_LIMIT - 1]).toBe(HISTORY_LIMIT + 4);
    expect(store.memoryHistory).toHaveLength(HISTORY_LIMIT);
  });

  it('tracks memory as a percentage of total', () => {
    const store = useMonitorStore();
    store.startWatch();
    streams[0].emit('metrics', sample());
    expect(store.memoryHistory).toEqual([25]);
    expect(store.memoryPercent).toBe(25);
  });

  it('does not push GPU points when the host has no GPU', () => {
    const store = useMonitorStore();
    store.startWatch();
    streams[0].emit('metrics', sample());
    // Pushing 0 here would draw a flat line that reads as an idle card.
    expect(store.gpuHistory).toEqual([]);
  });

  it('tracks GPU history when a GPU is present', () => {
    const store = useMonitorStore();
    store.startWatch();
    streams[0].emit('metrics', sample({
      gpu: { name: 'M3', usage: 42, memoryTotal: 100, memoryUsed: 10, temperatureC: 50 },
    }));
    expect(store.gpuHistory).toEqual([42]);
  });

  it('switches to polling after three consecutive stream errors (D45)', async () => {
    // Spelled out rather than derived from the constant: a loop of
    // `MAX_STREAM_ERRORS` iterations passes for *any* threshold, so it could not
    // catch the budget being changed. D45 says three.
    expect(MAX_STREAM_ERRORS).toBe(3);

    const store = useMonitorStore();
    store.startWatch();
    const es = streams[0];
    es.open();
    expect(store.watchMode).toBe('live');

    es.fail();
    es.fail();
    // Still under the budget: realtime has not been given up on.
    expect(store.watchMode).toBe('live');
    expect(fetchMetrics).not.toHaveBeenCalled();

    es.fail();
    expect(store.watchMode).toBe('polling');
    expect(es.closed).toBe(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchMetrics).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS);
    expect(fetchMetrics).toHaveBeenCalledTimes(2);
  });

  it('a recovered blip does not spend the error budget', () => {
    const store = useMonitorStore();
    store.startWatch();
    const es = streams[0];

    es.fail();
    es.fail();
    es.open(); // reconnected — counter resets
    es.fail();
    es.fail();

    expect(store.watchMode).toBe('live');
    expect(fetchMetrics).not.toHaveBeenCalled();
  });

  it('polling keeps feeding history', async () => {
    const store = useMonitorStore();
    store.startWatch();
    for (let i = 0; i < MAX_STREAM_ERRORS; i += 1) streams[0].fail();

    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS);
    expect(store.cpuHistory).toEqual([10, 10]);
  });

  it('stopWatch stops the poll timer too', async () => {
    const store = useMonitorStore();
    store.startWatch();
    for (let i = 0; i < MAX_STREAM_ERRORS; i += 1) streams[0].fail();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchMetrics).toHaveBeenCalledTimes(1);

    store.stopWatch();
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 3);
    expect(fetchMetrics).toHaveBeenCalledTimes(1);
    expect(store.watchMode).toBe('idle');
  });

  it('startWatch is idempotent', () => {
    const store = useMonitorStore();
    store.startWatch();
    store.startWatch();
    expect(systemEventSource).toHaveBeenCalledTimes(1);
  });

  it('startWatch does not open a stream while polling', async () => {
    const store = useMonitorStore();
    store.startWatch();
    for (let i = 0; i < MAX_STREAM_ERRORS; i += 1) streams[0].fail();
    await vi.advanceTimersByTimeAsync(0);

    store.startWatch();
    expect(systemEventSource).toHaveBeenCalledTimes(1);
    expect(store.watchMode).toBe('polling');
  });

  it('setView re-opens the stream with the new params', () => {
    const store = useMonitorStore();
    store.startWatch();
    expect(systemEventSource.mock.calls[0][0]).toMatchObject({ topN: 30, sort: 'cpu' });

    store.setView({ topN: 50, sort: 'memory' });
    expect(systemEventSource).toHaveBeenCalledTimes(2);
    expect(streams[0].closed).toBe(true);
    expect(systemEventSource.mock.calls[1][0]).toMatchObject({ topN: 50, sort: 'memory' });
  });

  it('setView does not open a stream when none was watching', () => {
    const store = useMonitorStore();
    store.setView({ sort: 'memory' });
    expect(systemEventSource).not.toHaveBeenCalled();
    expect(store.sort).toBe('memory');
  });

  it('filters processes by name, cmd, and exact pid without re-sorting', () => {
    const store = useMonitorStore();
    store.startWatch();
    streams[0].emit('metrics', sample({
      processes: [proc(1, 'zsh', '/bin/zsh', 9), proc(2, 'node', 'node server.js', 3), proc(33, 'rg', 'rg needle', 1)],
      processCount: 3,
    }));

    // Untouched: the order is the server's top-N-by-CPU cut.
    expect(store.processes.map((p) => p.pid)).toEqual([1, 2, 33]);

    store.filter = 'node';
    expect(store.processes.map((p) => p.pid)).toEqual([2]);
    store.filter = 'server.js';
    expect(store.processes.map((p) => p.pid)).toEqual([2]);
    store.filter = '33';
    expect(store.processes.map((p) => p.pid)).toEqual([33]);
    store.filter = 'ZSH';
    expect(store.processes.map((p) => p.pid)).toEqual([1]);
  });

  it('ignores an unparseable frame instead of killing the stream', () => {
    const store = useMonitorStore();
    store.startWatch();
    const es = streams[0];
    es.emit('metrics', sample());
    es.emit('metrics', 'not json');
    expect(store.cpuHistory).toEqual([10]);
    expect(es.closed).toBe(false);
  });

  it('kill refreshes so the row disappears before the next sample', async () => {
    const store = useMonitorStore();
    const ok = await store.kill(42, 'kill');
    expect(ok).toBe(true);
    expect(killProcess).toHaveBeenCalledWith(42, 'kill');
    expect(fetchMetrics).toHaveBeenCalledTimes(1);
    expect(store.busy).toBe(false);
  });

  it('kill surfaces a refusal instead of throwing', async () => {
    killProcess.mockRejectedValue(new Error('refusing to kill pid 1'));
    const store = useMonitorStore();
    const ok = await store.kill(1);
    expect(ok).toBe(false);
    expect(store.error).toBe('refusing to kill pid 1');
    expect(store.busy).toBe(false);
  });

  it('reset clears the sample and every history series', () => {
    const store = useMonitorStore();
    store.startWatch();
    streams[0].emit('metrics', sample({
      gpu: { name: 'M3', usage: 5, memoryTotal: 1, memoryUsed: 0, temperatureC: 40 },
    }));
    store.filter = 'x';

    store.reset();
    expect(store.metrics).toBeNull();
    expect(store.cpuHistory).toEqual([]);
    expect(store.memoryHistory).toEqual([]);
    expect(store.gpuHistory).toEqual([]);
    expect(store.filter).toBe('');
    expect(store.watchMode).toBe('idle');
    expect(streams[0].closed).toBe(true);
  });
});
