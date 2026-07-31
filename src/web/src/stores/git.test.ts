// Unit tests for the git store (C44, C45).
//
// Both rules here are about what happens when something goes wrong, which is
// exactly what an integration test cannot arrange: an `EventSource` that fails
// three times, and a commit button pressed with an empty box. So the stream is a
// fake with the parts the store actually uses, and the API module is mocked to
// assert what was *not* called.

import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import type { GitStatus } from '../api/git';
import { MAX_STREAM_ERRORS, POLL_INTERVAL_MS, useGitStore } from './git';

const {
  fetchStatus,
  fetchBranches,
  fetchLog,
  fetchDiff,
  commit,
  stageContent,
  unstageContent,
  discardContent,
  gitEventSource,
} = vi.hoisted(() => ({
  fetchStatus: vi.fn(),
  fetchBranches: vi.fn(),
  // Mocked even though no test asserts on it: a successful commit reloads the log,
  // so leaving it real would send the store through `fetch` and fail the commit
  // test with "window is not defined" — an error about the test environment, not
  // about the behaviour under test.
  fetchLog: vi.fn(),
  fetchDiff: vi.fn(),
  commit: vi.fn(),
  stageContent: vi.fn(),
  unstageContent: vi.fn(),
  discardContent: vi.fn(),
  gitEventSource: vi.fn(),
}));

vi.mock('../api/git', async () => {
  const actual = await vi.importActual<typeof import('../api/git')>('../api/git');
  return {
    ...actual,
    fetchStatus,
    fetchBranches,
    fetchLog,
    fetchDiff,
    commit,
    stageContent,
    unstageContent,
    discardContent,
    gitEventSource,
  };
});

const PROJECT = 'p1';

/**
 * A stand-in for `EventSource` exposing only what the store touches.
 *
 * jsdom has no `EventSource`, and even where it exists it needs a real server.
 * Driving the callbacks directly is what makes "three errors, then poll"
 * expressible as a test at all.
 */
class FakeEventSource {
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  private listeners = new Map<string, ((event: unknown) => void)[]>();

  addEventListener(type: string, fn: (event: unknown) => void): void {
    const existing = this.listeners.get(type) ?? [];
    this.listeners.set(type, [...existing, fn]);
  }

  close(): void {
    this.closed = true;
  }

  /** Deliver a named event, as the server's `event: status` frame would. */
  emit(type: string, data: string): void {
    for (const fn of this.listeners.get(type) ?? []) fn({ data });
  }

  fail(): void {
    this.onerror?.();
  }

  open(): void {
    this.onopen?.();
  }
}

function cleanStatus(overrides: Partial<GitStatus> = {}): GitStatus {
  return {
    isRepo: true,
    head: { branch: 'main', detached: false, oid: 'abc1234' },
    upstream: null,
    state: 'clean',
    entries: [],
    counts: { staged: 0, changed: 0, untracked: 0, conflicted: 0 },
    ...overrides,
  };
}

function cleanDiff(overrides: Partial<import('../api/git').GitDiff> = {}): import('../api/git').GitDiff {
  return {
    path: 'a.txt',
    staged: false,
    binary: false,
    patch: '@@ -1 +1 @@\n-old\n+new\n',
    oldText: 'old\n',
    newText: 'new\n',
    oldExists: true,
    newExists: true,
    worktreeOid: 'a'.repeat(40),
    added: 1,
    removed: 1,
    truncated: false,
    ...overrides,
  };
}

describe('git store', () => {
  let stream: FakeEventSource;

  beforeEach(() => {
    setActivePinia(createPinia());
    vi.useFakeTimers();
    fetchStatus.mockReset().mockResolvedValue(cleanStatus());
    fetchBranches.mockReset().mockResolvedValue({ current: 'main', local: [], remote: [] });
    commit.mockReset().mockResolvedValue(cleanStatus());
    fetchLog.mockReset().mockResolvedValue({ commits: [], nextBefore: null });
    fetchDiff.mockReset().mockResolvedValue(cleanDiff());
    stageContent.mockReset().mockResolvedValue(cleanStatus());
    unstageContent.mockReset().mockResolvedValue(cleanStatus());
    discardContent.mockReset().mockResolvedValue(cleanStatus());
    stream = new FakeEventSource();
    gitEventSource.mockReset().mockReturnValue(stream);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('applies a status frame from the stream', () => {
    const store = useGitStore();
    store.startWatch(PROJECT);

    const dirty = cleanStatus({
      entries: [
        {
          path: 'a.txt',
          origPath: null,
          index: 'none',
          worktree: 'modified',
          conflicted: false,
          staged: false,
        },
      ],
      counts: { staged: 0, changed: 1, untracked: 0, conflicted: 0 },
    });
    stream.open();
    stream.emit('status', JSON.stringify(dirty));

    expect(store.watchMode).toBe('live');
    expect(store.status.entries).toHaveLength(1);
    expect(store.status.entries[0].worktree).toBe('modified');
  });

  it('survives a frame it cannot parse', () => {
    // A stream is long-lived; one bad frame must not end it, because the next
    // frame carries the whole state anyway.
    const store = useGitStore();
    store.startWatch(PROJECT);
    stream.open();

    stream.emit('status', 'not json{');
    expect(store.error).toBeNull();

    stream.emit('status', JSON.stringify(cleanStatus()));
    expect(store.status.isRepo).toBe(true);
  });

  it('falls back to polling after three consecutive stream errors', () => {
    // C44. Two errors are a flaky network; three is an endpoint that is not
    // coming back, and retrying forever would leave a stale panel looking live.
    const store = useGitStore();
    store.startWatch(PROJECT);
    stream.open();

    for (let i = 0; i < MAX_STREAM_ERRORS - 1; i += 1) stream.fail();
    expect(store.watchMode).toBe('live');
    expect(stream.closed).toBe(false);

    stream.fail();
    expect(store.watchMode).toBe('polling');
    expect(stream.closed).toBe(true);

    // And it really polls, rather than just relabelling itself.
    const before = fetchStatus.mock.calls.length;
    vi.advanceTimersByTime(POLL_INTERVAL_MS * 2);
    expect(fetchStatus.mock.calls.length).toBeGreaterThan(before);
  });

  it('resets the error budget when the stream reopens', () => {
    // C44's other half: `EventSource` reconnects on its own, so a blip that
    // recovers must not spend the budget — otherwise a long session on a flaky
    // network eventually falls back for no reason.
    const store = useGitStore();
    store.startWatch(PROJECT);

    stream.fail();
    stream.fail();
    stream.open(); // recovered
    stream.fail();
    stream.fail();

    expect(store.watchMode).toBe('live');
    expect(stream.closed).toBe(false);
  });

  it('switches to polling when the server says its watcher stopped', () => {
    // The server gave up (C34). There is nothing left to listen to, so waiting
    // for three errors would just delay the fallback.
    const store = useGitStore();
    store.startWatch(PROJECT);
    stream.open();

    stream.emit('stopped', 'git status failed 3 times');

    expect(store.watchMode).toBe('polling');
    expect(stream.closed).toBe(true);
  });

  it('opens one stream per project and tears down on switch', () => {
    const store = useGitStore();
    store.startWatch(PROJECT);
    store.startWatch(PROJECT);
    expect(gitEventSource).toHaveBeenCalledTimes(1);

    const second = new FakeEventSource();
    gitEventSource.mockReturnValue(second);
    store.startWatch('p2');

    expect(gitEventSource).toHaveBeenCalledTimes(2);
    expect(stream.closed).toBe(true);
  });

  it('stops the poll timer on stopWatch', () => {
    const store = useGitStore();
    store.startWatch(PROJECT);
    for (let i = 0; i < MAX_STREAM_ERRORS; i += 1) stream.fail();
    expect(store.watchMode).toBe('polling');

    store.stopWatch();
    const before = fetchStatus.mock.calls.length;
    vi.advanceTimersByTime(POLL_INTERVAL_MS * 3);

    expect(store.watchMode).toBe('idle');
    expect(fetchStatus.mock.calls.length).toBe(before);
  });

  it('refuses an empty or whitespace-only commit message without calling the API', async () => {
    // C45. The server refuses it too, but a round-trip to be told "message is
    // required" is a worse answer than an immediate one.
    const store = useGitStore();

    for (const message of ['', '   ', '\n\t ']) {
      const ok = await store.commitStaged(PROJECT, message);
      expect(ok).toBe(false);
      expect(store.error).toContain('không được để trống');
    }
    expect(commit).not.toHaveBeenCalled();
  });

  it('commits a real message and adopts the returned status', async () => {
    const store = useGitStore();
    const ok = await store.commitStaged(PROJECT, 'feat: something');

    expect(ok).toBe(true);
    expect(commit).toHaveBeenCalledWith(PROJECT, 'feat: something', false);
    expect(store.status.isRepo).toBe(true);
    expect(store.error).toBeNull();
  });

  it('surfaces a failed mutation as an error and keeps the old status', async () => {
    const store = useGitStore();
    store.status = cleanStatus({ state: 'merge' });
    commit.mockRejectedValue(new Error('nothing to commit'));

    const ok = await store.commitStaged(PROJECT, 'feat: x');

    expect(ok).toBe(false);
    expect(store.error).toBe('nothing to commit');
    expect(store.status.state).toBe('merge');
    expect(store.busy).toBe(false);
  });

  it('does not ask for branches when the directory is not a repository', async () => {
    // C47's data half: a plain folder is information, not an error, and asking
    // for branches would turn it into one.
    fetchStatus.mockResolvedValue({
      isRepo: false,
      head: null,
      upstream: null,
      state: 'clean',
      entries: [],
      counts: { staged: 0, changed: 0, untracked: 0, conflicted: 0 },
    });
    const store = useGitStore();

    await store.refresh(PROJECT);

    expect(store.isRepo).toBe(false);
    expect(fetchBranches).not.toHaveBeenCalled();
    expect(store.error).toBeNull();
  });

  it('reports a detached HEAD in the branch label', () => {
    const store = useGitStore();
    store.status = cleanStatus({
      head: { branch: null, detached: true, oid: 'abcdef1234567890' },
    });
    expect(store.branchLabel).toBe('detached @ abcdef1');
  });

  // ---- hunk actions -------------------------------------------------------

  it('stageHunk calls stageContent with correct args and refreshes the worktree diff', async () => {
    const store = useGitStore();
    store.diffPath = 'a.txt';
    store.diffStaged = false;

    const ok = await store.stageHunk(PROJECT, 'a.txt', 'new content\n');

    expect(ok).toBe(true);
    expect(stageContent).toHaveBeenCalledWith(PROJECT, 'a.txt', 'new content\n');
    expect(fetchDiff).toHaveBeenCalledWith(PROJECT, 'a.txt', false);
  });

  it('stageHunk does not refresh when the mutation fails', async () => {
    stageContent.mockRejectedValue(new Error('hash failed'));
    const store = useGitStore();
    store.diffPath = 'a.txt';
    store.diffStaged = false;

    const ok = await store.stageHunk(PROJECT, 'a.txt', 'content\n');

    expect(ok).toBe(false);
    expect(fetchDiff).not.toHaveBeenCalled();
  });

  it('unstageHunk calls unstageContent with exists flag and refreshes the staged diff', async () => {
    const store = useGitStore();
    store.diffPath = 'a.txt';
    store.diffStaged = true;

    const ok = await store.unstageHunk(PROJECT, 'a.txt', 'idx content\n', true);

    expect(ok).toBe(true);
    expect(unstageContent).toHaveBeenCalledWith(PROJECT, 'a.txt', 'idx content\n', true);
    expect(fetchDiff).toHaveBeenCalledWith(PROJECT, 'a.txt', true);
  });

  it('unstageHunk passes exists=false for a newly-added file', async () => {
    const store = useGitStore();
    store.diffPath = 'fresh.txt';
    store.diffStaged = true;

    await store.unstageHunk(PROJECT, 'fresh.txt', '', false);

    expect(unstageContent).toHaveBeenCalledWith(PROJECT, 'fresh.txt', '', false);
  });

  it('discardHunk calls discardContent with expectedOid and refreshes the worktree diff', async () => {
    const oid = 'b'.repeat(40);
    const store = useGitStore();
    store.diffPath = 'a.txt';
    store.diffStaged = false;

    const ok = await store.discardHunk(PROJECT, 'a.txt', 'reverted\n', oid);

    expect(ok).toBe(true);
    expect(discardContent).toHaveBeenCalledWith(PROJECT, 'a.txt', 'reverted\n', oid);
    expect(fetchDiff).toHaveBeenCalledWith(PROJECT, 'a.txt', false);
  });

  it('discardHunk surfaces a stale-oid 409 as an error without refreshing', async () => {
    discardContent.mockRejectedValue(new Error('file changed after the diff was loaded; refresh before discarding a hunk'));
    const store = useGitStore();
    store.diffPath = 'a.txt';
    store.diffStaged = false;

    const ok = await store.discardHunk(PROJECT, 'a.txt', 'old\n', 'c'.repeat(40));

    expect(ok).toBe(false);
    expect(store.error).toContain('refresh before discarding');
    expect(fetchDiff).not.toHaveBeenCalled();
  });

  it('refreshOpenDiff does not re-fetch when a different diff is open', async () => {
    const store = useGitStore();
    store.diffPath = 'b.txt';
    store.diffStaged = false;

    await store.stageHunk(PROJECT, 'a.txt', 'content\n');

    expect(fetchDiff).not.toHaveBeenCalled();
  });
});
