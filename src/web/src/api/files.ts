// REST calls for the file tree and file contents (SPEC-002 §3.3–§3.5).
//
// Paths are ALWAYS project-relative and `/`-separated: the server rejects
// absolute paths and `..` outright (§3.6), and `path` travels as a query param
// so a nested path needs no segment-by-segment encoding.

import { apiFetch, ApiError } from './client';

export type EntryKind = 'dir' | 'file' | 'symlink';

/** One child of a listed directory. */
export interface TreeEntry {
  name: string;
  /** Relative to the project root — feeds straight back into `path`. */
  path: string;
  kind: EntryKind;
  /** Files only; absent for dirs and symlinks. */
  size?: number;
  mtimeMs: number;
}

export interface DirListing {
  path: string;
  /** True when the directory had more than `TREE_ENTRY_CAP` entries. */
  truncated: boolean;
  entries: TreeEntry[];
}

/**
 * Result of a read. The `kind` discriminant decides what the UI can do:
 * `binary` and `tooLarge` come back `200` WITH metadata but WITHOUT content
 * ([INVENTED-7]), so the pane shows "PNG, 20 KB — can't open" instead of an
 * error state that says nothing.
 */
export type ReadResult =
  | {
      kind: 'text';
      path: string;
      size: number;
      mtimeMs: number;
      rev: string;
      eol: 'lf' | 'crlf' | 'mixed';
      content: string;
    }
  | { kind: 'binary'; path: string; size: number; mtimeMs: number; rev: string; mime: string }
  | { kind: 'tooLarge'; path: string; size: number; mtimeMs: number; rev: string; mime: string };

export interface WriteResult {
  rev: string;
  size: number;
  mtimeMs: number;
}

/** `?path=` for a project-scoped file route. An empty path means the root. */
function fileUrl(projectId: string, route: string, path: string): string {
  const base = `/api/projects/${encodeURIComponent(projectId)}/${route}`;
  return `${base}?path=${encodeURIComponent(path)}`;
}

/** List one directory. `path` empty = project root (§3.3, lazy by design). */
export function readTree(projectId: string, path = ''): Promise<DirListing> {
  return apiFetch<DirListing>(fileUrl(projectId, 'tree', path));
}

export function readFile(projectId: string, path: string): Promise<ReadResult> {
  return apiFetch<ReadResult>(fileUrl(projectId, 'file', path));
}

/**
 * Write an existing file.
 *
 * `rev` is the optimistic-concurrency tag from the read: pass it and a file
 * changed on disk since gets a `409` instead of being clobbered. Omit it only
 * for a deliberate force-overwrite ([INVENTED-9]).
 */
export function writeFile(
  projectId: string,
  path: string,
  content: string,
  rev?: string,
): Promise<WriteResult> {
  return apiFetch<WriteResult>(fileUrl(projectId, 'file', path), {
    method: 'PUT',
    body: JSON.stringify(rev === undefined ? { content } : { content, rev }),
  });
}

export function createEntry(
  projectId: string,
  path: string,
  kind: 'file' | 'dir',
): Promise<{ path: string; kind: string }> {
  return apiFetch(`/api/projects/${encodeURIComponent(projectId)}/entries`, {
    method: 'POST',
    body: JSON.stringify({ path, kind }),
  });
}

export function renameEntry(
  projectId: string,
  path: string,
  newPath: string,
): Promise<{ path: string }> {
  return apiFetch(fileUrl(projectId, 'entries', path), {
    method: 'PATCH',
    body: JSON.stringify({ newPath }),
  });
}

/** Delete a file, or a directory (`recursive` required when non-empty, §3.5). */
export function deleteEntry(projectId: string, path: string, recursive = false): Promise<void> {
  const url = `${fileUrl(projectId, 'entries', path)}&recursive=${recursive}`;
  return apiFetch<void>(url, { method: 'DELETE' });
}

/**
 * The on-disk `rev` when a write fails with 409, so the UI can offer "Ghi đè"
 * against a known target. `null` when the error was anything else.
 */
export function conflictRev(err: unknown): string | null {
  if (!(err instanceof ApiError) || err.status !== 409) return null;
  return err.body && typeof err.body.currentRev === 'string' ? err.body.currentRev : null;
}
