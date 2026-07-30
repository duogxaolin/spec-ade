// HTTP + WebSocket plumbing shared by every feature panel.
//
// Auth (deep-dive 02 §4.4): the server opens the browser at `/?token=<token>`.
// We capture that token once, keep it in sessionStorage, and strip it from the
// visible URL so it doesn't linger in history. REST calls send it as a header;
// WebSocket upgrades can't set headers from a browser, so those pass `?token=`.

/** Header carrying the session token — must match `auth::TOKEN_HEADER`. */
export const TOKEN_HEADER = 'x-spec-ade-token';
/** sessionStorage key. Session-scoped on purpose: a token is not durable state. */
const TOKEN_STORAGE_KEY = 'spec_ade_token';

let cachedToken: string | null = null;

/**
 * Resolve the session token: `?token=` on first load, sessionStorage afterwards.
 *
 * Called for its side effect on startup, then memoized — reading the URL twice
 * would return nothing the second time, since we clean the query string.
 */
export function resolveToken(): string {
  if (cachedToken !== null) return cachedToken;

  const url = new URL(window.location.href);
  const fromUrl = url.searchParams.get('token');
  if (fromUrl) {
    sessionStorage.setItem(TOKEN_STORAGE_KEY, fromUrl);
    url.searchParams.delete('token');
    window.history.replaceState({}, '', url.toString());
    cachedToken = fromUrl;
    return fromUrl;
  }

  cachedToken = sessionStorage.getItem(TOKEN_STORAGE_KEY) ?? '';
  return cachedToken;
}

/** Error carrying the HTTP status, so callers can branch on 401/404. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    /**
     * The parsed error body, when there was one. Some errors carry fields the
     * caller needs to act on rather than just display: a 409 from
     * `POST /api/projects` includes `existingId`, and one from
     * `PUT .../file` includes `currentRev` (SPEC-002 §3.2, §3.4).
     */
    readonly body: Record<string, unknown> | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/**
 * `fetch` with the token attached and a non-2xx turned into an `ApiError`.
 *
 * The server's error bodies are `{error, detail}`; we surface `detail` because
 * it's the part that says what actually went wrong (e.g. which cwd was bad).
 */
export async function apiFetch<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = resolveToken();
  const headers = new Headers(init.headers);
  if (token) headers.set(TOKEN_HEADER, token);
  if (init.body && !headers.has('content-type')) {
    headers.set('content-type', 'application/json');
  }

  const res = await fetch(path, { ...init, headers });

  if (!res.ok) {
    let detail = `HTTP ${res.status}`;
    let body: Record<string, unknown> | null = null;
    try {
      const parsed = (await res.json()) as Record<string, unknown>;
      if (parsed && typeof parsed === 'object') {
        body = parsed;
        const fromBody = parsed.detail ?? parsed.error;
        if (typeof fromBody === 'string') detail = fromBody;
      }
    } catch {
      // Non-JSON error body (e.g. a proxy's HTML page) — keep the status text.
    }
    throw new ApiError(res.status, detail, body);
  }

  // 204 No Content has no body to parse.
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

/**
 * Absolute ws:// URL for an API path, with the token as a query param.
 *
 * Derived from `window.location` so it works unchanged in three deployments:
 * the embedded SPA (same origin), the Vite dev server (proxied), and the Tauri
 * WebView.
 */
export function wsUrl(path: string, params: Record<string, string | number | undefined> = {}): string {
  const url = new URL(path, window.location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';

  const token = resolveToken();
  if (token) url.searchParams.set('token', token);
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) url.searchParams.set(key, String(value));
  }
  return url.toString();
}
