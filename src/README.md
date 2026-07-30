# src/ — Spec ADE monorepo

From-scratch reimplementation of Spec ADE, structured as a monorepo. The roadmap
and the architecture notes this layout mirrors live in a private `docs/` tree that
is not published; paths cited below and in code comments resolve only in a local
working copy.

## Layout

```
src/
├── server/    Rust backend — a single Axum binary (spec-ade-server)
├── web/       Vue 3 + Quasar + Pinia + TypeScript frontend (Vite)
└── desktop/   Tauri v2 shell — SKELETON ONLY (embeds server as a sidecar)
```

### `server/` — the backend

One Axum binary is the whole backend. It serves the embedded SPA, exposes REST
for CRUD, WebSocket for terminal I/O + chat streaming, and SSE for git watch —
all on **one origin** (no CORS, simplifies Tauri/PWA). Maps to the "Axum server
(single binary)" box in the architecture diagram.

Module map. Modules not yet built carry a doc comment plus a `// TODO(phase-N)`
marker instead of an implementation:

| Module      | Responsibility                                        | State |
|-------------|-------------------------------------------------------|-------|
| `main.rs`   | Bind host/port, mount router, mint the auth token     | done  |
| `auth.rs`   | Token gate + origin check on every `/api/*` route     | done  |
| `spa.rs`    | Serve the embedded SPA, history-mode fallback         | done  |
| `routes/`   | REST/WS/SSE handlers                                  | partial |
| `pty/`      | PTY spawn + blocking-read threads → mpsc → WS         | done  |
| `acp/`      | ACP client + reverse JSON-RPC server, replayable log  | done  |
| `settings/` | JSON config load/save, partial update                 | done  |
| `storage/`  | `~/.config/spec-ade` layout                           | done  |
| `git/`      | git2 reads + CLI mutations, watch                     | stub  |
| `search/`   | streaming content search                              | stub  |
| `claws/`    | scheduled autonomous agents                           | stub  |

Only the crates in use are active in `Cargo.toml`; later ones are listed but gated
by their phase.

### `web/` — the frontend

Vue 3 + Quasar SPA built by Vite. In production it is **built and embedded** into
the server binary (via `rust-embed`); in dev it runs on the Vite dev server proxying
`/api` to the backend. Shipped so far: terminal (xterm.js), file tree + CodeMirror 6
editor, settings, and the ACP pane. Pinia setup-style stores under `src/stores`,
with non-reactive handles (WebSocket, xterm, CodeMirror) held outside `ref`.

### `desktop/` — the Tauri shell (skeleton)

Not a reimplementation of the backend — it ships `spec-ade-server` as a
**sidecar** and points a WebView at `http://127.0.0.1:<port>`. Full Tauri
scaffolding is deferred to Pha 9. See [`desktop/README.md`](desktop/README.md).

## Intended build/run flow

```
# Dev (two processes, one origin via Vite proxy)
cd src/server && cargo run          # backend on :4123 (SPEC_ADE_HOST/PORT env)
cd src/web    && npm run dev        # SPA dev server, proxies /api → :4123

# Production (single binary)
cd src/web    && npm run build      # emit web/dist
cd src/server && cargo build --release   # embeds web/dist, one static binary

# Desktop (Pha 9)
# build the SPA + server, drop the server into desktop as a target-triple
# sidecar, then `tauri build`.
```

## Security note (do not skip)

PTY/ACP over WebSocket **without auth is remote code execution by design** —
binding `127.0.0.1` is not enough (DNS-rebinding / CSRF-on-WS). Every route except
`/api/health` is gated by a token, and the origin check runs first so a hostile
origin is rejected before any PTY is spawned.

## Next steps

Chat UI (markdown streaming + tool visualization), then git, search + process
monitor, scheduled agents, panes, and the Tauri shell.
