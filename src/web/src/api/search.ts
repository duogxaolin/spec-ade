// Typed client for the search endpoint (SPEC-006 §3.1).
//
// One function, because search is one route — but the query string is the whole
// contract: `glob` is **repeatable** (`?glob=*.rs&glob=*.ts`), which is why this
// builds the URL with `append` rather than an object literal. `URLSearchParams`
// with a record would keep only the last value, exactly the bug the server-side
// `form_urlencoded` parsing exists to avoid.

import { resolveToken } from './client';

/** One matching **line** — a line with three hits is one event with three ranges. */
export interface SearchMatch {
  /** Project-relative, `/`-separated: the same shape `GET …/file` takes. */
  path: string;
  /** 1-based. */
  line: number;
  /** The line without its terminator, truncated server-side to 4096 bytes. */
  text: string;
  /** `[start, end)` byte offsets into `text`. Empty when the line is not UTF-8. */
  ranges: Array<[number, number]>;
}

export interface SearchProgress {
  filesScanned: number;
  matches: number;
}

export interface SearchDone {
  matches: number;
  /** Files that contained at least one match. */
  files: number;
  filesScanned: number;
  /**
   * The cap was hit. Not a promise that exactly `maxResults` arrived — the
   * parallel walk stops asynchronously, so a few extra matches can trail in.
   */
  truncated: boolean;
  elapsedMs: number;
}

/** A single file failed to read. Non-fatal: the walk continued. */
export interface SearchFileError {
  path: string;
  detail: string;
}

/** Everything the query box can set. */
export interface SearchParams {
  query: string;
  /** Treat `query` as a regex. Default: literal. */
  regex?: boolean;
  /** Case-**sensitive**. Default: insensitive, like an editor's search box. */
  case?: boolean;
  word?: boolean;
  /** Plain glob whitelists, `!glob` excludes. */
  globs?: string[];
  /** Project-relative subdirectory to search in. */
  path?: string;
  maxResults?: number;
}

/**
 * Open the search stream.
 *
 * Returned rather than wrapped so the caller owns the lifecycle: the store must
 * `close()` the previous stream before opening the next one, which is also the
 * cancellation signal the server listens for (§5.4, D41).
 */
export function searchEventSource(projectId: string, params: SearchParams): EventSource {
  const url = new URL(
    `/api/projects/${encodeURIComponent(projectId)}/search`,
    window.location.href,
  );
  const q = url.searchParams;
  q.set('query', params.query);
  if (params.regex) q.set('regex', 'true');
  if (params.case) q.set('case', 'true');
  if (params.word) q.set('word', 'true');
  if (params.path) q.set('path', params.path);
  if (params.maxResults !== undefined) q.set('maxResults', String(params.maxResults));
  for (const glob of params.globs ?? []) {
    if (glob.trim() !== '') q.append('glob', glob);
  }

  // `EventSource` cannot set headers, so the token rides as a query param — the
  // same compromise `wsUrl` makes.
  const token = resolveToken();
  if (token) q.set('token', token);

  return new EventSource(url.toString());
}

/**
 * Split a comma/space-separated glob field into globs.
 *
 * The input is one text box because two boxes (include/exclude) would still need
 * this, and `!` already expresses the difference.
 */
export function parseGlobs(input: string): string[] {
  return input
    .split(/[\s,]+/)
    .map((g) => g.trim())
    .filter((g) => g !== '');
}
