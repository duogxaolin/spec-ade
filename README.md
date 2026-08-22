# Spec ADE

A from-scratch reimplementation of Spec ADE — a browser-based agentic development
environment. A single Rust binary (Axum) serves an embedded Vue 3 SPA and speaks
[ACP](https://github.com/zed-industries/agent-client-protocol) over stdio to
coding agents such as `claude` or `codex`.

Built as a study project: each subsystem is implemented from a written spec, then
verified against the real release binary before the next one starts.

## Status

| Area | State |
|------|-------|
| Skeleton — Axum, embedded SPA, auth, WS echo | done |
| Terminal — PTY over WebSocket | done |
| File tree + editor (CodeMirror 6), settings + projects CRUD | done |
| ACP orchestration — spawn agent, stream updates, `?after_seq=N` replay | done |
| Chat UI — markdown, syntax highlighting, diffs, math, mermaid | done |
| Git — status, diff, log, branches, conflict resolution, SSE watch | done |
| Search — streaming ripgrep across projects | done |
| Process monitor — live metrics, kill | done |
| Claws — scheduled autonomous agents (cron), skills | done |
| Pane system + layout, Tauri shell, licensing/PWA | planned |

Current gates: `cargo fmt --check`, `clippy -D warnings` clean, 407 backend
tests, 546 frontend tests, `vue-tsc --noEmit` clean, and six runtime scripts
(296 checks total) driving the release binary over HTTP + WebSocket + SSE.

Design notes, per-subsystem specs and the running status log are kept in a private
`docs/` tree and are not published. Code comments cite paths under `docs/`; those
resolve only in a local working copy.

## Layout

```
src/
├── server/   Rust — Axum, PTY, ACP client, storage, embedded SPA
├── web/      Vue 3 + Quasar + Pinia frontend (embedded into the binary)
└── desktop/  Tauri v2 shell (sidecar, not started)
scripts/      Runtime verification scripts + reference-repo setup
```

## Build and run

Requires a Rust toolchain (2024 edition) and Node 22+.

```sh
cd src/web    && npm ci && npm run build   # emits src/web/dist, embedded below
cd ../server  && cargo build --release
./target/release/spec-ade-server
```

The frontend must be built first: the server embeds `src/web/dist` at compile
time via `rust-embed`, so a stale or missing `dist` ships a stale or missing UI.

## Security

Terminal and agent access over WebSocket is **remote code execution by design**.
Two things follow:

- Every `/api/*` route except `/api/health` requires an auth token (a UUID v4 kept
  in `~/.config/spec-ade/settings.json`, generated on first run). The token is
  enforced on localhost too, because binding loopback does not stop DNS-rebinding
  or CSRF-on-WebSocket. An origin check runs before the token check, so a hostile
  origin is rejected before any process is spawned.
- **The default bind host is `0.0.0.0`**, so out of the box the server is reachable
  from the local network and anyone holding the token gets a shell. On an untrusted
  network, bind loopback explicitly:

  ```sh
  ./target/release/spec-ade-server --host 127.0.0.1
  ```

The startup banner prints a URL with the token embedded so the SPA can authenticate
on first load; treat that URL as a credential.

Tests:

```sh
cd src/server && cargo test                        # 407
cd ../web     && npx vitest run                    # 546
cd ../..      && for s in scripts/verify-spec-*.mjs; do node "$s"; done   # 296 checks
```

## Setting up a fresh clone

```sh
cp .mcp.json.example .mcp.json   # then fill in the local port + path
```

`.mcp.json` is gitignored: its URL embeds a localhost port and the checkout's
absolute path, both machine-specific. `.mcp.json.example` documents how to build
the value. Nothing else is needed — `src/` builds and tests on its own.

`scripts/clone-references.sh` restores the private study archive (12 upstream repos
under `docs/references`, plus the upstream docs). It is only useful alongside the
unpublished `docs/` tree, and none of it is a build dependency.

## Third-party references (study only)

The repositories below were read while building this project. They are **not**
vendored, **not** dependencies, and no code is copied from them — they are cloned
locally on demand by `scripts/clone-references.sh` and retain their own licenses:

| Repo | Upstream | License |
|------|----------|---------|
| `agent-client-protocol` | zed-industries/agent-client-protocol | Apache-2.0 |
| `zed` | zed-industries/zed | Apache-2.0 (+ GPL for some crates) |
| `NotepadAI` | nullmastermind/NotepadAI | GPL-3.0 |
| `gitbutler` | gitbutlerapp/gitbutler | FSL-1.1-MIT |
| `ripgrep` | BurntSushi/ripgrep | Unlicense OR MIT |
| `tokio-cron-scheduler` | mvniekerk/tokio-cron-scheduler | Apache-2.0 OR MIT |
| `wezterm` | wez/wezterm | MIT |
| `ttyd` | tsl0922/ttyd | MIT |
| `sshx` | ekzhang/sshx | MIT |
| `bottom` | ClementTsang/bottom | MIT |
| `esbuild` | evanw/esbuild | MIT |

Licenses were read from each upstream checkout rather than assumed; `zed`,
`ripgrep` and `tokio-cron-scheduler` ship more than one license file, and
`gitbutler` uses the Functional Source License, which is source-available rather
than open source. Their terms govern their own contents. The implementation under
`src/` is independent.
