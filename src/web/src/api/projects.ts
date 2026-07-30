// REST calls for the project registry (SPEC-002 §3.2).
//
// `id` is a UUID, not the path ([INVENTED-4]): paths contain `/` and would need
// double percent-encoding to sit in a path segment.

import { apiFetch, ApiError } from './client';

/** Mirrors the server's `ProjectEntry`. */
export interface Project {
  id: string;
  /** Canonical absolute path — unique across the registry. */
  path: string;
  name: string;
  icon: string | null;
  sortOrder: number;
}

export interface CreateProjectRequest {
  path: string;
  name?: string;
  icon?: string;
}

/** Same absent/null/value semantics as the settings patch. */
export interface UpdateProjectRequest {
  name?: string | null;
  icon?: string | null;
  sortOrder?: number | null;
}

export function listProjects(): Promise<Project[]> {
  return apiFetch<Project[]>('/api/projects');
}

export function createProject(body: CreateProjectRequest): Promise<Project> {
  return apiFetch<Project>('/api/projects', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function updateProject(id: string, body: UpdateProjectRequest): Promise<Project> {
  return apiFetch<Project>(`/api/projects/${encodeURIComponent(id)}`, {
    method: 'PUT',
    body: JSON.stringify(body),
  });
}

export function deleteProject(id: string): Promise<void> {
  return apiFetch<void>(`/api/projects/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

/**
 * The id already in the registry when a create fails with 409 (§3.2 puts it in
 * `existingId`), so the UI can select that project instead of just reporting a
 * clash. `null` when the error was anything else.
 */
export function duplicateProjectId(err: unknown): string | null {
  if (!(err instanceof ApiError) || err.status !== 409) return null;
  return err.body && typeof err.body.existingId === 'string' ? err.body.existingId : null;
}
