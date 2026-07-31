// Monitor store — the live sample, the sparkline history, and the poll fallback.
//
// Two rules with tests:
//
// - **60-point history** (D44). Every sample appends to three series; unbounded,
//   a tab left open overnight holds 28 800 points per series that nothing renders.
//   The oldest is dropped past [`HISTORY_LIMIT`].
// - **Poll fallback** (D45, same rule as C44). `EventSource` retries forever,
//   which silently shows a stale panel when the endpoint is broken rather than
//   flaky. Three consecutive errors close it and switch to `GET /metrics` on a
//   timer, and `watchMode` says so. `onopen` resets the counter, so a blip that
//   recovered does not spend the budget.
//
// The `EventSource` and the timer live outside reactive state, same rule as `git.ts`.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  fetchMetrics,
  killProcess as apiKillProcess,
  systemEventSource,
  DEFAULT_TOP_N,
  type KillSignalName,
  type Metrics,
  type ProcessInfo,
  type SortBy,
} from '../api/system';
import { pushPoint, HISTORY_LIMIT } from '../monitor/sparkline';

/** Consecutive `EventSource` errors before giving up on realtime (D45, C44). */
export const MAX_STREAM_ERRORS = 3;
/** Poll interval once realtime is off — matches the server's sample cadence. */
export const POLL_INTERVAL_MS = 3000;

export type WatchMode = 'idle' | 'live' | 'polling';

export const useMonitorStore = defineStore('monitor', () => {
  const metrics = ref<Metrics | null>(null);
  const watchMode = ref<WatchMode>('idle');
  const error = ref<string | null>(null);
  const busy = ref(false);

  const topN = ref(DEFAULT_TOP_N);
  const sort = ref<SortBy>('cpu');
  /** Substring filter over process name and command line. */
  const filter = ref('');

  const cpuHistory = ref<number[]>([]);
  const memoryHistory = ref<number[]>([]);
  const gpuHistory = ref<number[]>([]);

  const hasSample = computed(() => metrics.value !== null);
  const memoryPercent = computed(() => {
    const mem = metrics.value?.memory;
    if (!mem || mem.total === 0) return 0;
    return (mem.used / mem.total) * 100;
  });

  /**
   * Filtered process list. The sort and the top-N cut are the *server's* — asking
   * it for the top 30 by CPU and then re-sorting here would show a list whose
   * order does not match what was selected from.
   */
  const processes = computed<ProcessInfo[]>(() => {
    const all = metrics.value?.processes ?? [];
    const needle = filter.value.trim().toLowerCase();
    if (needle === '') return all;
    return all.filter(
      (p) =>
        p.name.toLowerCase().includes(needle) ||
        p.cmd.toLowerCase().includes(needle) ||
        String(p.pid) === needle,
    );
  });

  // Not reactive — see the module comment.
  let source: EventSource | null = null;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let streamErrors = 0;

  function applyMetrics(next: Metrics): void {
    metrics.value = next;
    cpuHistory.value = pushPoint(cpuHistory.value, next.cpu.usage);
    const mem = next.memory.total === 0 ? 0 : (next.memory.used / next.memory.total) * 100;
    memoryHistory.value = pushPoint(memoryHistory.value, mem);
    // Only tracked when the host has a GPU; an absent one must not push zeroes
    // and draw a flat line that looks like an idle card.
    if (next.gpu) gpuHistory.value = pushPoint(gpuHistory.value, next.gpu.usage);
  }

  async function refresh(): Promise<void> {
    try {
      applyMetrics(await fetchMetrics({ topN: topN.value, sort: sort.value }));
      error.value = null;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
    }
  }

  function stopPolling(): void {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  function closeSource(): void {
    if (source !== null) {
      source.close();
      source = null;
    }
  }

  /** Give up on realtime and poll `GET /metrics` instead (D45). */
  function startPolling(): void {
    closeSource();
    if (pollTimer !== null) return;
    watchMode.value = 'polling';
    void refresh();
    pollTimer = setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);
  }

  /**
   * Open the metrics stream.
   *
   * Idempotent: a second call while a stream or a poll timer is live is a no-op,
   * so a component remounting does not open a second stream — which on the server
   * side would be a second subscriber, not a second sampler, but still a socket
   * nobody reads.
   */
  function startWatch(): void {
    if (source !== null || pollTimer !== null) return;
    streamErrors = 0;

    const es = systemEventSource({ topN: topN.value, sort: sort.value });
    source = es;

    es.onopen = () => {
      streamErrors = 0;
      watchMode.value = 'live';
    };

    es.addEventListener('metrics', (event) => {
      try {
        applyMetrics(JSON.parse((event as MessageEvent<string>).data) as Metrics);
        watchMode.value = 'live';
        error.value = null;
      } catch {
        // An unparseable frame is not worth killing the stream over; the next one
        // carries the whole state anyway.
      }
    });

    es.onerror = () => {
      streamErrors += 1;
      if (streamErrors >= MAX_STREAM_ERRORS) {
        startPolling();
      }
    };
  }

  function stopWatch(): void {
    closeSource();
    stopPolling();
    streamErrors = 0;
    watchMode.value = 'idle';
  }

  /**
   * Re-open the stream with new query params.
   *
   * `topN` and `sort` are applied server-side, so changing them means a new
   * subscription — there is no way to renegotiate an open `EventSource`.
   */
  function setView(next: { topN?: number; sort?: SortBy }): void {
    if (next.topN !== undefined) topN.value = next.topN;
    if (next.sort !== undefined) sort.value = next.sort;
    const wasWatching = source !== null || pollTimer !== null;
    stopWatch();
    if (wasWatching) startWatch();
  }

  /**
   * Send a signal to a process.
   *
   * The confirmation belongs to the component (D46), not here: a store method
   * that opens a dialog cannot be called from anywhere else.
   */
  async function kill(pid: number, signal: KillSignalName = 'term'): Promise<boolean> {
    busy.value = true;
    error.value = null;
    try {
      await apiKillProcess(pid, signal);
      // The next sample is up to 3s away; refresh so the row disappears now.
      await refresh();
      return true;
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      busy.value = false;
    }
  }

  function reset(): void {
    stopWatch();
    metrics.value = null;
    cpuHistory.value = [];
    memoryHistory.value = [];
    gpuHistory.value = [];
    filter.value = '';
    error.value = null;
  }

  return {
    metrics,
    watchMode,
    error,
    busy,
    topN,
    sort,
    filter,
    cpuHistory,
    memoryHistory,
    gpuHistory,
    hasSample,
    memoryPercent,
    processes,
    historyLimit: HISTORY_LIMIT,
    refresh,
    startWatch,
    stopWatch,
    setView,
    kill,
    reset,
  };
});
