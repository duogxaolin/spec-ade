// Typed client for the claws routes (SPEC-007 §3.1–§3.4).
//
// The DTOs below mirror `src/server/src/claws/mod.rs` and
// `src/server/src/routes/claws.rs` field-for-field — same rule as `api/git.ts`
// and `api/system.ts`: one file states both shapes so a server rename breaks
// this diff, not a runtime `undefined`.
//
// Error groups come back as `{error, detail}` (+ `schedule` for cron errors,
// §3.2); `apiFetch` surfaces `detail`, and `ApiError.body.error` carries the
// group (`agent` / `project` / `cron` / `claw`) for branching.

import { apiFetch } from './client';

/** How a Claw answers the agent's permission requests (§5.2). */
export type PermissionMode = 'auto_approve' | 'deny_all' | 'ask_via_ui';

/** One cron entry (`claws.mdx:63-70`). Stored verbatim — never re-rendered. */
export interface ClawSchedule {
  label?: string | null;
  /** The string the user typed; the server normalises it on save. */
  cron: string;
  prompts: string[];
  enabled: boolean;
}

/** A persisted Claw (§3.1). Lives in `settings.claws`. */
export interface ClawDefinition {
  id: string;
  name: string;
  agentId: string;
  projectId: string;
  /** `null` = a Claw that only runs its schedule prompts ([INVENTED-4]). */
  skill: string | null;
  enabled: boolean;
  autoStart: boolean;
  keepAlive: boolean;
  restartOnTrigger: boolean;
  permissionMode: PermissionMode;
  skipIfRunning: boolean;
  schedules: ClawSchedule[];
}

/** Lifecycle (`claws.mdx:39-48`), snake_case exactly as the server serializes it. */
export type ClawState = 'stopped' | 'starting' | 'running' | 'idle' | 'error';

/** The read-only runtime view merged into every `GET /api/claws` row. */
export interface ClawStatus {
  state: ClawState;
  connectionId: string | null;
  sessionId: string | null;
  restarts: number;
  lastRunAt: string | null;
  lastError: string | null;
  /** Computed fresh per GET from the server's clock ([INVENTED-11]). */
  nextRunAt: string | null;
  scheduleCount: number;
  /** One human rendering per schedule, in order (§3.3). */
  scheduleDescriptions: string[];
}

/** A `GET` row: the definition flattened with `status` merged in. */
export type ClawRow = ClawDefinition & { status: ClawStatus };

/** The `POST`/`PUT` body — the whole definition minus `id` (§3.2 full replace). */
export interface ClawInput {
  name: string;
  agentId: string;
  projectId: string;
  skill?: string | null;
  enabled: boolean;
  autoStart: boolean;
  keepAlive: boolean;
  restartOnTrigger: boolean;
  permissionMode: PermissionMode;
  skipIfRunning: boolean;
  schedules: ClawSchedule[];
}

/** Where a skill was found — workspace copies win over user ones. */
export type SkillSource = 'workspace' | 'user';

/** One discovered `SKILL.md` (§3.4). */
export interface Skill {
  /** The directory name — the identity a Claw stores. */
  name: string;
  source: SkillSource;
  dir: string;
  description?: string | null;
  license?: string | null;
  compatibility?: string | null;
  allowedTools?: string | null;
  metadata?: unknown;
  /** The body after the frontmatter — what the Claw actually sends. */
  prompt: string;
}

export function listClaws(projectId?: string): Promise<ClawRow[]> {
  const q = new URLSearchParams();
  if (projectId) q.set('projectId', projectId);
  const qs = q.toString();
  return apiFetch<ClawRow[]>(`/api/claws${qs ? `?${qs}` : ''}`);
}

export function getClaw(id: string): Promise<ClawRow> {
  return apiFetch<ClawRow>(`/api/claws/${encodeURIComponent(id)}`);
}

export function createClaw(input: ClawInput): Promise<ClawRow> {
  return apiFetch<ClawRow>('/api/claws', {
    method: 'POST',
    body: JSON.stringify(input),
  });
}

/** Full replace (§3.2) — send every field, not a patch. */
export function updateClaw(id: string, input: ClawInput): Promise<ClawRow> {
  return apiFetch<ClawRow>(`/api/claws/${encodeURIComponent(id)}`, {
    method: 'PUT',
    body: JSON.stringify(input),
  });
}

export function deleteClaw(id: string): Promise<void> {
  return apiFetch<void>(`/api/claws/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

/** Start now — already running is a 409, spawn failure is a 502 with stderr. */
export function startClaw(id: string): Promise<{ status: ClawStatus }> {
  return apiFetch(`/api/claws/${encodeURIComponent(id)}/start`, { method: 'POST' });
}

/** Stop — idempotent (E20). */
export function stopClaw(id: string): Promise<{ status: ClawStatus }> {
  return apiFetch(`/api/claws/${encodeURIComponent(id)}/stop`, { method: 'POST' });
}

/** Re-scanned per call — no cache, new skills appear without a restart. */
export function listSkills(projectId: string): Promise<Skill[]> {
  return apiFetch<Skill[]>(
    `/api/projects/${encodeURIComponent(projectId)}/skills`,
  );
}
