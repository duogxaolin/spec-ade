// Git store — status, diff, log, branches, plus the live watch and its fallback.
//
// The `EventSource` lives outside reactive state, the same rule `terminals.ts`
// applies to xterm and `acp.ts` to its sockets: nothing renders from the stream
// object, and wrapping it in a Vue proxy buys only surprises.
//
// Two behaviours here are load-bearing and both have tests:
//
// - **Poll fallback** (C44). `EventSource` reconnects on its own, which is usually
//   what you want and is exactly wrong when the endpoint is broken rather than
//   flaky: it retries forever while the panel silently shows stale state. So three
//   consecutive errors close it and switch to `setInterval` polling, and the UI
//   says so — a user who knows realtime is off will refresh; one who doesn't will
//   trust a stale panel. `onopen` resets the counter, so a network blip that
//   recovers does not spend the budget.
// - **Empty commit message** (C45). Refused here, before the request. The server
//   also refuses it, but a round-trip to be told "message is required" is a worse
//   answer than an immediate one, and the button state comes from the same rule.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  abortMerge as apiAbortMerge,
  checkout as apiCheckout,
  commit as apiCommit,
  createBranch as apiCreateBranch,
  discard as apiDiscard,
  discardContent as apiDiscardContent,
  fetchBranches,
  fetchCommit,
  fetchConflict,
  fetchDiff,
  fetchLog,
  fetchStatus,
  gitEventSource,
  merge as apiMerge,
  resolveConflict as apiResolveConflict,
  stage as apiStage,
  stageContent as apiStageContent,
  unstageContent as apiUnstageContent,
  type CommitDetail,
  type GitBranches,
  type GitConflict,
  type GitDiff,
  type GitStatus,
  type Commit,
} from '../api/git';
import { hasStagedWork } from '../git/status';

/** Consecutive `EventSource` errors before giving up on realtime (§5.7, C44). */
export const MAX_STREAM_ERRORS = 3;
/** Poll interval once realtime is off (06:67). */
export const POLL_INTERVAL_MS = 3000;
/** Commits per log page. */
const LOG_PAGE = 30;

/** How the panel is currently learning about changes. */
export type WatchMode = 'idle' | 'live' | 'polling';

/** The status of a project with no repository — what `isRepo:false` looks like. */
function emptyStatus(): GitStatus {
  return {
    isRepo: false,
    head: null,
    upstream: null,
    state: 'clean',
    entries: [],
    counts: { staged: 0, changed: 0, untracked: 0, conflicted: 0 },
  };
}

export const useGitStore = defineStore('git', () => {
  const status = ref<GitStatus>(emptyStatus());
  const branches = ref<GitBranches>({ current: null, local: [], remote: [] });
  const commits = ref<Commit[]>([]);
  /** Cursor for the next log page; `null` means the history ended. */
  const nextBefore = ref<string | null>(null);
  const diff = ref<GitDiff | null>(null);
  const commitDetail = ref<CommitDetail | null>(null);
  const conflict = ref<GitConflict | null>(null);

  const loading = ref(false);
  const busy = ref(false);
  const error = ref<string | null>(null);
  const watchMode = ref<WatchMode>('idle');

  /** Which path the diff pane is showing, and from which side (C9). */
  const diffPath = ref<string | null>(null);
  const diffStaged = ref(false);

  const isRepo = computed(() => status.value.isRepo);
  const canCommit = computed(() => hasStagedWork(status.value.entries));
  const hasConflicts = computed(() => status.value.counts.conflicted > 0);
  const branchLabel = computed(() => {
    const head = status.value.head;
    if (!head) return '';
    if (head.detached) return head.oid ? `detached @ ${head.oid.slice(0, 7)}` : 'detached';
    return head.branch ?? '';
  });

  // Not reactive — see the module comment.
  let source: EventSource | null = null;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let streamErrors = 0;
  /** The project the stream belongs to, so a switch can be detected. */
  let watchedProject: string | null = null;

  // ---- reads ---------------------------------------------------------------

  async function refresh(projectId: string): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      status.value = await fetchStatus(projectId);
      // Branches only exist if the directory is a repository; asking anyway would
      // turn a plain folder into an error banner (C5, C47).
      if (status.value.isRepo) {
        branches.value = await fetchBranches(projectId);
      }
    } catch (err) {
      error.value = messageOf(err);
    } finally {
      loading.value = false;
    }
  }

  /** First log page, replacing whatever was there. */
  async function loadLog(projectId: string, path?: string): Promise<void> {
    error.value = null;
    try {
      const page = await fetchLog(projectId, { limit: LOG_PAGE, path });
      commits.value = page.commits;
      nextBefore.value = page.nextBefore;
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  /**
   * Append the next page.
   *
   * The cursor is the oid of the commit *after* the page, and the server returns
   * it inclusively — so the first commit of the next page is the cursor itself,
   * and appending blindly would duplicate one row. Dedupe by oid rather than
   * slicing, because a concurrent commit can shift the boundary.
   */
  async function loadMore(projectId: string, path?: string): Promise<void> {
    const before = nextBefore.value;
    if (!before) return;
    error.value = null;
    try {
      const page = await fetchLog(projectId, { limit: LOG_PAGE, before, path });
      const seen = new Set(commits.value.map((c) => c.oid));
      commits.value = [...commits.value, ...page.commits.filter((c) => !seen.has(c.oid))];
      nextBefore.value = page.nextBefore;
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  /** Show one file's diff. `staged` picks index-vs-HEAD over worktree-vs-index. */
  async function openDiff(projectId: string, path: string, staged: boolean): Promise<void> {
    diffPath.value = path;
    diffStaged.value = staged;
    error.value = null;
    try {
      diff.value = await fetchDiff(projectId, path, staged);
    } catch (err) {
      diff.value = null;
      error.value = messageOf(err);
    }
  }

  function closeDiff(): void {
    diff.value = null;
    diffPath.value = null;
  }

  async function openCommit(projectId: string, oid: string): Promise<void> {
    error.value = null;
    try {
      commitDetail.value = await fetchCommit(projectId, oid);
    } catch (err) {
      commitDetail.value = null;
      error.value = messageOf(err);
    }
  }

  function closeCommit(): void {
    commitDetail.value = null;
  }

  async function openConflict(projectId: string, path: string): Promise<void> {
    error.value = null;
    try {
      conflict.value = await fetchConflict(projectId, path);
    } catch (err) {
      conflict.value = null;
      error.value = messageOf(err);
    }
  }

  function closeConflict(): void {
    conflict.value = null;
  }

  // ---- mutations -----------------------------------------------------------

  /**
   * Run a mutation and adopt the status it returns.
   *
   * Every mutation answers with the fresh status ([SPEC-005 INVENTED-6]), so this
   * never needs a follow-up read — which matters when an agent is editing the same
   * files: a separate `GET status` could observe a state neither the mutation nor
   * the user produced.
   */
  async function mutate(run: () => Promise<GitStatus>): Promise<boolean> {
    busy.value = true;
    error.value = null;
    try {
      status.value = await run();
      return true;
    } catch (err) {
      error.value = messageOf(err);
      return false;
    } finally {
      busy.value = false;
    }
  }

  function stagePaths(projectId: string, paths: string[]): Promise<boolean> {
    if (paths.length === 0) return Promise.resolve(false);
    return mutate(() => apiStage(projectId, paths, false));
  }

  function unstagePaths(projectId: string, paths: string[]): Promise<boolean> {
    if (paths.length === 0) return Promise.resolve(false);
    return mutate(() => apiStage(projectId, paths, true));
  }

  function discardPaths(projectId: string, paths: string[]): Promise<boolean> {
    if (paths.length === 0) return Promise.resolve(false);
    return mutate(() => apiDiscard(projectId, paths));
  }

  async function refreshOpenDiff(projectId: string, path: string, staged: boolean): Promise<void> {
    if (diffPath.value !== path || diffStaged.value !== staged) return;
    try {
      diff.value = await fetchDiff(projectId, path, staged);
    } catch (err) {
      diff.value = null;
      diffPath.value = null;
      error.value = messageOf(err);
    }
  }

  /** Apply one hunk's document to the index and keep the open diff current. */
  async function stageHunk(projectId: string, path: string, content: string): Promise<boolean> {
    const ok = await mutate(() => apiStageContent(projectId, path, content));
    if (ok) await refreshOpenDiff(projectId, path, false);
    return ok;
  }

  /** Apply one hunk's document (or absence) to the index. */
  async function unstageHunk(
    projectId: string,
    path: string,
    content: string,
    exists: boolean,
  ): Promise<boolean> {
    const ok = await mutate(() => apiUnstageContent(projectId, path, content, exists));
    if (ok) await refreshOpenDiff(projectId, path, true);
    return ok;
  }

  /** Replace only the worktree document after CodeMirror rejects one chunk. */
  async function discardHunk(
    projectId: string,
    path: string,
    content: string,
    expectedOid: string,
  ): Promise<boolean> {
    const ok = await mutate(() => apiDiscardContent(projectId, path, content, expectedOid));
    if (ok) await refreshOpenDiff(projectId, path, false);
    return ok;
  }

  /**
   * Commit the index (C45).
   *
   * An empty or whitespace-only message never reaches the API: git would refuse it
   * too, but the answer belongs next to the button that produced it, and the same
   * predicate disables that button.
   */
  async function commitStaged(
    projectId: string,
    message: string,
    amend = false,
  ): Promise<boolean> {
    if (message.trim().length === 0) {
      error.value = 'Commit message không được để trống.';
      return false;
    }
    const ok = await mutate(() => apiCommit(projectId, message, amend));
    if (ok) await loadLog(projectId);
    return ok;
  }

  async function newBranch(
    projectId: string,
    name: string,
    opts: { startPoint?: string; checkout?: boolean } = {},
  ): Promise<boolean> {
    if (name.trim().length === 0) {
      error.value = 'Tên branch không được để trống.';
      return false;
    }
    const ok = await mutate(() => apiCreateBranch(projectId, name.trim(), opts));
    if (ok) branches.value = await fetchBranches(projectId);
    return ok;
  }

  async function switchTo(projectId: string, target: string, force = false): Promise<boolean> {
    const ok = await mutate(() => apiCheckout(projectId, target, force));
    if (ok) {
      branches.value = await fetchBranches(projectId);
      await loadLog(projectId);
    }
    return ok;
  }

  async function mergeFrom(projectId: string, from: string, noFf = false): Promise<boolean> {
    const ok = await mutate(() => apiMerge(projectId, from, noFf));
    if (ok) await loadLog(projectId);
    return ok;
  }

  function abortMergeNow(projectId: string): Promise<boolean> {
    return mutate(() => apiAbortMerge(projectId));
  }

  async function resolvePath(
    projectId: string,
    path: string,
    content: string,
  ): Promise<boolean> {
    const ok = await mutate(() => apiResolveConflict(projectId, path, content));
    if (ok && conflict.value?.path === path) conflict.value = null;
    return ok;
  }

  // ---- watch ---------------------------------------------------------------

  function applyStatus(next: GitStatus): void {
    status.value = next;
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

  /**
   * Give up on realtime and poll instead (C44).
   *
   * Polling is a worse experience, so `watchMode` becomes `'polling'` and the panel
   * shows it: a user who can see realtime is off will hit refresh when it matters.
   */
  function startPolling(projectId: string): void {
    closeSource();
    if (pollTimer !== null) return;
    watchMode.value = 'polling';
    pollTimer = setInterval(() => {
      void refresh(projectId);
    }, POLL_INTERVAL_MS);
  }

  /**
   * Open the watch stream for `projectId`.
   *
   * Idempotent per project: calling it again for the project already watched is a
   * no-op, so a component remounting does not open a second stream. Switching
   * project tears the old one down first.
   */
  function startWatch(projectId: string): void {
    if (watchedProject === projectId && (source !== null || pollTimer !== null)) return;
    stopWatch();

    watchedProject = projectId;
    streamErrors = 0;

    const es = gitEventSource(projectId);
    source = es;

    es.onopen = () => {
      // A blip that recovered must not count against the budget, or a long session
      // on a flaky network eventually falls back for no reason.
      streamErrors = 0;
      watchMode.value = 'live';
    };

    es.addEventListener('status', (event) => {
      try {
        applyStatus(JSON.parse((event as MessageEvent<string>).data) as GitStatus);
        watchMode.value = 'live';
      } catch {
        // A frame we cannot parse is not worth killing the stream over; the next
        // one carries the whole state anyway.
      }
    });

    // The server sends this when its watcher gave up (C34 server-side): there is
    // nothing left to listen to, so switch without waiting for an error.
    es.addEventListener('stopped', () => {
      startPolling(projectId);
    });

    es.onerror = () => {
      streamErrors += 1;
      if (streamErrors >= MAX_STREAM_ERRORS) {
        startPolling(projectId);
      }
    };
  }

  function stopWatch(): void {
    closeSource();
    stopPolling();
    watchedProject = null;
    streamErrors = 0;
    watchMode.value = 'idle';
  }

  /** Everything the panel holds for one project — dropped on a project switch. */
  function reset(): void {
    stopWatch();
    status.value = emptyStatus();
    branches.value = { current: null, local: [], remote: [] };
    commits.value = [];
    nextBefore.value = null;
    diff.value = null;
    diffPath.value = null;
    diffStaged.value = false;
    commitDetail.value = null;
    conflict.value = null;
    error.value = null;
  }

  function dismissError(): void {
    error.value = null;
  }

  return {
    status,
    branches,
    commits,
    nextBefore,
    diff,
    diffPath,
    diffStaged,
    commitDetail,
    conflict,
    loading,
    busy,
    error,
    watchMode,
    isRepo,
    canCommit,
    hasConflicts,
    branchLabel,
    refresh,
    loadLog,
    loadMore,
    openDiff,
    closeDiff,
    openCommit,
    closeCommit,
    openConflict,
    closeConflict,
    stagePaths,
    unstagePaths,
    discardPaths,
    stageHunk,
    unstageHunk,
    discardHunk,
    commitStaged,
    newBranch,
    switchTo,
    mergeFrom,
    abortMergeNow,
    resolvePath,
    startWatch,
    stopWatch,
    reset,
    dismissError,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
