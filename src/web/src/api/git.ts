// Typed client for the git endpoints (SPEC-005 §3) + the watch EventSource.
//
// The DTOs mirror `src/server/src/git/repo.rs` field for field. They are written
// out rather than inferred because the server's `#[serde(rename_all)]` makes the
// wire names camelCase while the Rust names are snake_case — the only place the
// two shapes are stated together is here, so a rename on either side has to be
// reconciled in one file.
//
// `index` and `worktree` are two independent axes, not one merged state: a file
// can be staged and modified again, which `git status` calls `MM` (C3, C42).

import { apiFetch, resolveToken } from './client';

/** Per-axis change state. `none` means "unchanged on this axis". */
export type ChangeState = 'none' | 'new' | 'modified' | 'deleted' | 'renamed' | 'typechange';

/** What a repository is in the middle of. `clean` means no operation pending. */
export type RepoState =
  | 'clean'
  | 'merge'
  | 'revert'
  | 'cherryPick'
  | 'bisect'
  | 'rebase'
  | 'apply';

export interface HeadInfo {
  /** `null` on a detached HEAD (C6). */
  branch: string | null;
  detached: boolean;
  /** `null` on an unborn branch — a repo with no commits yet. */
  oid: string | null;
}

export interface UpstreamInfo {
  name: string;
  ahead: number;
  behind: number;
}

export interface StatusEntry {
  path: string;
  /** A rename's source path, when this entry is a rename. */
  origPath: string | null;
  index: ChangeState;
  worktree: ChangeState;
  conflicted: boolean;
  /** Convenience mirror of `index !== 'none'`. */
  staged: boolean;
}

export interface StatusCounts {
  staged: number;
  changed: number;
  untracked: number;
  conflicted: number;
}

export interface GitStatus {
  /** `false` for a plain directory — information, not an error (C5). */
  isRepo: boolean;
  head: HeadInfo | null;
  upstream: UpstreamInfo | null;
  state: RepoState;
  entries: StatusEntry[];
  counts: StatusCounts;
}

export interface GitDiff {
  path: string;
  staged: boolean;
  /** `true` → `patch`/`oldText`/`newText` are empty by design (C10). */
  binary: boolean;
  patch: string;
  oldText: string;
  newText: string;
  /** Whether each side has a path (distinct from an existing empty file). */
  oldExists: boolean;
  newExists: boolean;
  /** Blob id of the worktree snapshot, used to reject stale discard-hunk writes. */
  worktreeOid: string | null;
  added: number;
  removed: number;
  /** The file exceeded the server's size ceiling ([SPEC-005 INVENTED-3]). */
  truncated: boolean;
}

export interface Signature {
  name: string;
  email: string;
  /** Unix seconds. */
  time: number;
}

export interface Commit {
  oid: string;
  short: string;
  summary: string;
  body: string;
  author: Signature;
  parents: string[];
}

export interface GitLog {
  commits: Commit[];
  /** Cursor for the next page, `null` at the end of history ([INVENTED-7]). */
  nextBefore: string | null;
}

export interface CommitFile {
  path: string;
  origPath: string | null;
  change: string;
  added: number;
  removed: number;
}

export interface CommitDetail {
  commit: Commit;
  files: CommitFile[];
}

export interface LocalBranch {
  name: string;
  oid: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  current: boolean;
}

export interface RemoteBranch {
  name: string;
  oid: string | null;
}

export interface GitBranches {
  current: string | null;
  local: LocalBranch[];
  remote: RemoteBranch[];
}

export interface BlameLine {
  line: number;
  oid: string;
  short: string;
  author: string;
  time: number;
  summary: string;
}

export interface GitBlame {
  path: string;
  lines: BlameLine[];
}

export interface GitBlob {
  path: string;
  rev: string;
  binary: boolean;
  content: string;
}

/** The three sides of a conflict. A side is `null` when it deleted the file. */
export interface GitConflict {
  path: string;
  base: string | null;
  ours: string | null;
  theirs: string | null;
  binary: boolean;
}

/** Which version of a file `blob` should read. */
export type BlobRev = 'HEAD' | 'index' | 'worktree' | (string & {});

function gitUrl(projectId: string, route: string, params: Record<string, string | number | boolean | undefined> = {}): string {
  const url = `/api/projects/${encodeURIComponent(projectId)}/git/${route}`;
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) query.set(key, String(value));
  }
  const suffix = query.toString();
  return suffix ? `${url}?${suffix}` : url;
}

// ---- reads -----------------------------------------------------------------

export function fetchStatus(projectId: string): Promise<GitStatus> {
  return apiFetch<GitStatus>(gitUrl(projectId, 'status'));
}

/**
 * One file's diff.
 *
 * `staged` picks the comparison, and the two answers differ for an `MM` file:
 * `true` is index-vs-HEAD, `false` is worktree-vs-index (C9).
 */
export function fetchDiff(projectId: string, path: string, staged = false): Promise<GitDiff> {
  return apiFetch<GitDiff>(gitUrl(projectId, 'diff', { path, staged }));
}

export function fetchLog(
  projectId: string,
  opts: { limit?: number; before?: string; path?: string } = {},
): Promise<GitLog> {
  return apiFetch<GitLog>(gitUrl(projectId, 'log', opts));
}

export function fetchCommit(projectId: string, oid: string): Promise<CommitDetail> {
  return apiFetch<CommitDetail>(
    `/api/projects/${encodeURIComponent(projectId)}/git/commit/${encodeURIComponent(oid)}`,
  );
}

export function fetchBranches(projectId: string): Promise<GitBranches> {
  return apiFetch<GitBranches>(gitUrl(projectId, 'branches'));
}

export function fetchBlame(projectId: string, path: string): Promise<GitBlame> {
  return apiFetch<GitBlame>(gitUrl(projectId, 'blame', { path }));
}

export function fetchBlob(projectId: string, path: string, rev: BlobRev = 'HEAD'): Promise<GitBlob> {
  return apiFetch<GitBlob>(gitUrl(projectId, 'blob', { path, rev }));
}

export function fetchConflict(projectId: string, path: string): Promise<GitConflict> {
  return apiFetch<GitConflict>(gitUrl(projectId, 'conflict', { path }));
}

// ---- mutations -------------------------------------------------------------
//
// Every mutation answers with the fresh status ([INVENTED-6]), so a caller never
// has to follow a write with a read — which matters when agents are editing files
// at the same time.

function post<T>(projectId: string, route: string, body: unknown): Promise<T> {
  return apiFetch<T>(gitUrl(projectId, route), {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function stage(projectId: string, paths: string[], unstage = false): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'stage', { paths, unstage });
}

/** Replace one index entry with the document produced by a Stage hunk action. */
export function stageContent(
  projectId: string,
  path: string,
  content: string,
): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'stage-content', { path, content });
}

/** Replace/remove one index entry after an Unstage hunk action. */
export function unstageContent(
  projectId: string,
  path: string,
  content: string,
  exists: boolean,
): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'unstage-content', { path, content, exists });
}

export function commit(projectId: string, message: string, amend = false): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'commit', { message, amend });
}

export function discard(projectId: string, paths: string[]): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'discard', { paths });
}

/** Atomically replace the worktree document produced by a Discard hunk action. */
export function discardContent(
  projectId: string,
  path: string,
  content: string,
  expectedOid: string,
): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'discard-content', { path, content, expectedOid });
}

export function createBranch(
  projectId: string,
  name: string,
  opts: { startPoint?: string; checkout?: boolean } = {},
): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'branch', {
    name,
    startPoint: opts.startPoint,
    checkout: opts.checkout ?? false,
  });
}

export function checkout(projectId: string, target: string, force = false): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'checkout', { target, force });
}

export function merge(projectId: string, from: string, noFf = false): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'merge', { from, noFf });
}

export function abortMerge(projectId: string): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'merge', { abort: true });
}

export function resolveConflict(
  projectId: string,
  path: string,
  content: string,
): Promise<GitStatus> {
  return post<GitStatus>(projectId, 'resolve', { path, content });
}

// ---- watch (SSE) -----------------------------------------------------------

/**
 * Open the watch stream.
 *
 * `EventSource` cannot set headers, so the token rides as a query param — the
 * same compromise the WebSocket routes make (`client.ts` `wsUrl`), and the server
 * accepts `?token=` for exactly this reason.
 *
 * Returned rather than wrapped so the caller owns the lifecycle: the store needs
 * `onerror`/`onopen` itself to run the poll fallback (§5.7, C44).
 */
export function gitEventSource(projectId: string): EventSource {
  const url = new URL(gitUrl(projectId, 'watch'), window.location.href);
  const token = resolveToken();
  if (token) url.searchParams.set('token', token);
  return new EventSource(url.toString());
}
