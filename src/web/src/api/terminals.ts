// REST calls for the terminal surface (SPEC-001 §5.6).

import { apiFetch } from './client';

/** Mirrors the server's `TerminalInfo`. */
export interface TerminalInfo {
  id: string;
  pid: number | null;
  rows: number;
  cols: number;
  cwd: string;
  alive: boolean;
  /** Present once the shell is dead; null when it was killed by a signal. */
  exitCode: number | null;
  /** Output bytes produced so far. */
  seq: number;
}

export interface SpawnRequest {
  cwd?: string;
  rows?: number;
  cols?: number;
  shell?: string;
  args?: string[];
  env?: Record<string, string>;
}

export function spawnTerminal(body: SpawnRequest = {}): Promise<TerminalInfo> {
  return apiFetch<TerminalInfo>('/api/terminals', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function listTerminals(): Promise<TerminalInfo[]> {
  return apiFetch<TerminalInfo[]>('/api/terminals');
}

/** Kill the shell and drop it from the server's registry. */
export function killTerminal(id: string): Promise<void> {
  return apiFetch<void>(`/api/terminals/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}
