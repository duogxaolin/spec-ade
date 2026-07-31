// Typed client for the system routes (SPEC-006 §3.2–§3.4).
//
// The DTOs below are hand-written mirrors of `src/server/src/monitor/mod.rs` —
// same reasoning as `api/git.ts`: this is the one file where both shapes are
// stated together, so a rename on either side is a diff here, not a runtime
// `undefined` three components deep.

import { apiFetch, resolveToken } from './client';

/** Bytes everywhere. Percentages are 0–100, not 0–1. */
export interface CpuMetrics {
  /** Global usage across all cores, 0–100. */
  usage: number;
  coreCount: number;
  perCore: number[];
}

export interface MemoryMetrics {
  total: number;
  used: number;
  swapTotal: number;
  swapUsed: number;
}

export interface HostMetrics {
  name: string | null;
  os: string | null;
  uptimeSec: number;
  /** 1/5/15-minute load average. All zeroes on Windows. */
  loadAvg: [number, number, number];
}

export interface GpuMetrics {
  name: string;
  usage: number;
  memoryTotal: number;
  memoryUsed: number;
  temperatureC: number | null;
}

export interface ProcessInfo {
  pid: number;
  parentPid: number | null;
  name: string;
  cmd: string;
  /** Percent of **one** core — can exceed 100, exactly as `top` reports it. */
  cpu: number;
  /** Resident memory, bytes. */
  memory: number;
  status: string;
  runTimeSec: number;
  user: string | null;
}

export interface Metrics {
  timestampMs: number;
  cpu: CpuMetrics;
  memory: MemoryMetrics;
  host: HostMetrics;
  /** `null` when the host has no GPU — hide the section, do not show an error. */
  gpu: GpuMetrics | null;
  processes: ProcessInfo[];
  /** Total processes on the host, **not** `processes.length`. */
  processCount: number;
  truncated: boolean;
}

export type SortBy = 'cpu' | 'memory';

/** Server default (`DEFAULT_TOP_N`), repeated so the UI can show it before the first sample. */
export const DEFAULT_TOP_N = 30;

/** Sampler cadence in ms (`SAMPLE_INTERVAL`), used to size the history window. */
export const SAMPLE_INTERVAL_MS = 3000;

export interface MetricsQuery {
  topN?: number;
  sort?: SortBy;
}

function metricsSearch(query: MetricsQuery): URLSearchParams {
  const q = new URLSearchParams();
  if (query.topN !== undefined) q.set('topN', String(query.topN));
  if (query.sort) q.set('sort', query.sort);
  return q;
}

/** `GET /api/system/metrics` — also the poll fallback when the stream dies (D45). */
export function fetchMetrics(query: MetricsQuery = {}): Promise<Metrics> {
  const q = metricsSearch(query).toString();
  return apiFetch<Metrics>(`/api/system/metrics${q ? `?${q}` : ''}`);
}

/**
 * Open the metrics stream.
 *
 * Returned unwrapped for the same reason as `gitEventSource`: the store needs
 * `onopen`/`onerror` itself to run the poll fallback (§5.7, C44).
 */
export function systemEventSource(query: MetricsQuery = {}): EventSource {
  const url = new URL('/api/system/watch', window.location.href);
  const q = metricsSearch(query);
  const token = resolveToken();
  if (token) q.set('token', token);
  url.search = q.toString();
  return new EventSource(url.toString());
}

export type KillSignalName = 'term' | 'kill' | 'int' | 'hup';

export interface KillResult {
  ok: true;
  pid: number;
  signal: KillSignalName;
}

/**
 * `POST /api/system/kill/{pid}`.
 *
 * Defaults to `term`, not `kill` — a dev server deserves the chance to flush.
 * The caller is expected to have confirmed with the user first (D46).
 */
export function killProcess(pid: number, signal: KillSignalName = 'term'): Promise<KillResult> {
  return apiFetch<KillResult>(`/api/system/kill/${pid}`, {
    method: 'POST',
    body: JSON.stringify({ signal }),
  });
}
