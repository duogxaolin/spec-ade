// Search store — the query box, the stream, and the results (SPEC-006 §5.9).
//
// Three rules here are load-bearing and each has a test:
//
// - **Debounce, 200 ms** (D42). Every keystroke would otherwise open a walk over
//   the whole project. The server would cancel each one when its stream closes,
//   but "start a parallel walk, then cancel it" five times per word is work the
//   client can simply not ask for.
// - **Close before open** (D41). The old `EventSource` is closed *before* the new
//   one is created, because closing is the cancellation signal (§5.4): the
//   receiver drops, `tx.is_closed()` flips, and the walk quits. Opening first
//   would leave two walks racing to fill the same result list.
// - **A generation counter.** `close()` does not retract frames already dispatched
//   into the task queue, so a late `match` from the previous stream can arrive
//   after the new one started. Each stream carries a generation and frames from
//   an old one are dropped — without it a stale result appears under a new query.
//
// The `EventSource` itself lives outside reactive state, same rule as `git.ts`.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  searchEventSource,
  type SearchDone,
  type SearchFileError,
  type SearchMatch,
  type SearchParams,
  type SearchProgress,
} from '../api/search';
import { pushMatch, type FileGroup } from '../search/group';

/** Milliseconds of quiet before a query is sent (§5.9). */
export const DEBOUNCE_MS = 200;

/** Toggles the query box owns, kept together so a change is one reactive write. */
export interface SearchOptions {
  regex: boolean;
  case: boolean;
  word: boolean;
  globs: string[];
  path: string | null;
}

function defaultOptions(): SearchOptions {
  return { regex: false, case: false, word: false, globs: [], path: null };
}

export const useSearchStore = defineStore('search', () => {
  const query = ref('');
  const options = ref<SearchOptions>(defaultOptions());

  const groups = ref<FileGroup[]>([]);
  const errors = ref<SearchFileError[]>([]);
  const running = ref(false);
  /** The cap was hit — the UI says so rather than implying these are all results. */
  const truncated = ref(false);
  const matchCount = ref(0);
  const fileCount = ref(0);
  const filesScanned = ref(0);
  const elapsedMs = ref<number | null>(null);
  /** A request the server refused before opening the stream (bad regex, bad glob). */
  const error = ref<string | null>(null);

  const hasResults = computed(() => groups.value.length > 0);
  /** True once a search has finished and produced nothing — distinct from "not started". */
  const empty = computed(
    () => !running.value && elapsedMs.value !== null && groups.value.length === 0,
  );

  // Not reactive — see the module comment.
  let source: EventSource | null = null;
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  /** Incremented per stream; frames from an older generation are ignored. */
  let generation = 0;
  let watchedProject: string | null = null;

  function clearResults(): void {
    groups.value = [];
    errors.value = [];
    truncated.value = false;
    matchCount.value = 0;
    fileCount.value = 0;
    filesScanned.value = 0;
    elapsedMs.value = null;
    error.value = null;
  }

  /** Close the stream, which is also how the server is told to stop walking (§5.4). */
  function closeSource(): void {
    if (source !== null) {
      source.close();
      source = null;
    }
  }

  function cancelDebounce(): void {
    if (debounceTimer !== null) {
      clearTimeout(debounceTimer);
      debounceTimer = null;
    }
  }

  /**
   * Open a stream immediately, bypassing the debounce.
   *
   * Public because Enter in the query box should not wait 200 ms, and because the
   * debounced path is easier to reason about when it is just a timer around this.
   */
  function run(projectId: string): void {
    cancelDebounce();
    // Close first: this is the cancellation signal, and doing it after `new
    // EventSource` would leave two walks running against the same result list.
    closeSource();

    const text = query.value;
    if (text.trim() === '') {
      running.value = false;
      clearResults();
      return;
    }

    watchedProject = projectId;
    clearResults();
    running.value = true;

    const mine = ++generation;
    const params: SearchParams = {
      query: text,
      regex: options.value.regex,
      case: options.value.case,
      word: options.value.word,
      globs: options.value.globs,
      path: options.value.path ?? undefined,
    };

    const es = searchEventSource(projectId, params);
    source = es;

    /** Frames dispatched before `close()` can still land — drop the stale ones. */
    const fresh = (): boolean => mine === generation;

    es.addEventListener('match', (event) => {
      if (!fresh()) return;
      const match = parse<SearchMatch>(event);
      if (!match) return;
      // `groups.value` is a reactive proxy, so mutating it in place is what the
      // UI observes — reassigning a fresh array per match would re-render every
      // row 2000 times.
      pushMatch(groups.value, match);
      matchCount.value += 1;
    });

    es.addEventListener('progress', (event) => {
      if (!fresh()) return;
      const progress = parse<SearchProgress>(event);
      if (!progress) return;
      filesScanned.value = progress.filesScanned;
    });

    es.addEventListener('error', (event) => {
      // Two different things arrive on `error`: our own `event: error` frames,
      // which carry a JSON body naming one unreadable file, and the browser's
      // transport error, which has no data. Only the first is a file error.
      if (!fresh()) return;
      const fileError = parse<SearchFileError>(event);
      if (fileError && typeof fileError.path === 'string') {
        errors.value = [...errors.value, fileError];
      }
    });

    es.addEventListener('done', (event) => {
      if (!fresh()) return;
      const done = parse<SearchDone>(event);
      if (done) {
        matchCount.value = done.matches;
        fileCount.value = done.files;
        filesScanned.value = done.filesScanned;
        truncated.value = done.truncated;
        elapsedMs.value = done.elapsedMs;
      }
      running.value = false;
      // The server has finished; holding the socket open buys nothing.
      closeSource();
    });

    es.onerror = () => {
      if (!fresh()) return;
      // The stream ends with `done`, so an error here means the connection broke
      // before that — and `EventSource` would retry the whole search forever.
      running.value = false;
      closeSource();
    };
  }

  /**
   * Set the query and search after [`DEBOUNCE_MS`] of quiet (D42).
   *
   * The query is written immediately so the box stays responsive; only the
   * network call waits.
   */
  function search(projectId: string, next: string): void {
    query.value = next;
    schedule(projectId);
  }

  /** Re-run after a toggle change, on the same debounce. */
  function schedule(projectId: string): void {
    cancelDebounce();
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      run(projectId);
    }, DEBOUNCE_MS);
  }

  function setOptions(projectId: string, next: Partial<SearchOptions>): void {
    options.value = { ...options.value, ...next };
    if (query.value.trim() !== '') schedule(projectId);
  }

  /** Stop the current search without clearing what it already found. */
  function stop(): void {
    cancelDebounce();
    // Bump the generation so in-flight frames from the closed stream are dropped.
    generation += 1;
    closeSource();
    running.value = false;
  }

  function reset(): void {
    stop();
    query.value = '';
    options.value = defaultOptions();
    watchedProject = null;
    clearResults();
  }

  return {
    query,
    options,
    groups,
    errors,
    running,
    truncated,
    matchCount,
    fileCount,
    filesScanned,
    elapsedMs,
    error,
    hasResults,
    empty,
    search,
    setOptions,
    run,
    stop,
    reset,
    /** Test/debug only: which project the last stream was opened for. */
    watchedProject: () => watchedProject,
  };
});

/** Parse an SSE frame's data, returning null rather than throwing on garbage. */
function parse<T>(event: unknown): T | null {
  const data = (event as MessageEvent<string>).data;
  if (typeof data !== 'string' || data === '') return null;
  try {
    return JSON.parse(data) as T;
  } catch {
    return null;
  }
}
