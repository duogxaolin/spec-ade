# Agent Constitution

Rules that hold across every change to this repo. Each one exists because it was
either violated once and cost a real debugging session, or guards the single
biggest risk in the product. Trap-laden subsystems get a deep-dive under
[`docs/reference/`](./docs/reference/) — read it before touching that subsystem.

The constitution is deliberately small: every rule here earns its place by
having caught or prevented a real failure. When a debugging session uncovers a
new trap, it becomes a rule here — and, if the subsystem is trap-laden, a
deep-dive under [`docs/reference/`](./docs/reference/).

## Token gate: every `/api/*` route is authenticated, no exceptions except health

Any new `/api/*` route **must** be merged into `authed` in `build_router`
(`src/server/src/lib.rs`) so it inherits `require_auth` + `require_origin`. The
only open route is `/api/health`, mounted alongside `authed`, deliberately.

This is RCE-by-design territory: an unauthenticated PTY or ACP WebSocket lets any
web page the user visits spawn shells and drive agents. Loopback binding does not
save you (CSRF-on-WS, DNS rebinding). Full stakes and mechanics:
[`docs/reference/auth-token-gate.md`](./docs/reference/auth-token-gate.md).
The pin test `every_new_route_requires_the_token`
(`src/server/tests/search-monitor.rs`) exists to catch a forgotten merge — extend
it when adding a gated subsystem, never delete or weaken it.

## portable-pty is a blocking API

`portable-pty`'s reader, writer, and `Child::wait` are all **blocking**. Every
terminal owns dedicated `std::thread`s bridged into tokio via channels — never
`tokio::spawn` around these calls. `take_writer()` is valid exactly once.
Drop the slave handle before relying on the master seeing EOF. Details and
failure modes: [`docs/reference/pty-blocking-api.md`](./docs/reference/pty-blocking-api.md).

Same family, different subsystem: never hold a `std::sync::Mutex` guard across an
`.await` — it is not `Send` and deadlocks under contention; use `tokio::sync::Mutex`
when the guard must survive an await point.

## File and module naming

Never use vague names like `helpers`, `utils`, `common`, `misc`, or `shared-stuff`
for files, folders, or modules. They carry zero information and become dumping
grounds. Name files after what they actually contain — prefer the concrete domain
concept (`scrollback.rs`, `path_guard.rs`) over the generic role (`fs-utils.rs`).
If you reach for `helpers`, the file probably has more than one responsibility and
should be split, or a better name is hiding in the code describing what the
functions operate on.

Integration test files are named after the domain they exercise
(`bootstrap.rs`, `terminal.rs`, `projects-files.rs`, `acp-orchestration.rs`,
`git-integration.rs`, `search-monitor.rs`, `claws-scheduling.rs`) — never after a
build order (`phase0.rs`…). Build order is metadata, not identity; specs get
inserted and reordered, domains don't.

## Comments: WHY only, one line where possible

Do not explain the obvious or narrate the code ("what" is readable; "why" is not).
A comment earns its place by recording a constraint, a trap, a deliberate choice,
or a source citation (`unix.rs:357-364`, deep-dive section). If it could be
deleted with zero loss of understanding, delete it.

## Lint suppression is never the fix

CI runs `cargo clippy -- -D warnings` and `vue-tsc` with no errors allowed.
Never silence a warning with `#[allow(...)]`, `#[cfg_attr(..., allow(...))]`,
`eslint-disable`, `@ts-ignore`, or a config carve-out to make it go away. Fix the
root cause: an unused import gets removed, a redundant closure becomes a path
reference, a wrong signature gets declared correctly (four `as never` casts died
this way). If a lint fires on third-party macro output and cannot be fixed at the
call site, stop and surface the problem instead of suppressing it.

## The DONE bar

A spec is DONE only when **all three layers are green**, in this order:

1. **Build**: `cargo fmt --check`, `cargo build`, `cargo clippy --all-targets -- -D warnings`;
   `npm run build` in `src/web` (includes `vue-tsc --noEmit`).
2. **Test**: `cargo test` (unit for pure logic, integration against the compiled
   binary pattern), `npx vitest run`.
3. **Verify**: run the spec's `scripts/verify-spec-NNN.mjs` against the real
   built binary — HTTP/WS/SSE across the same boundary the browser uses.

No `todo!()`, no TODO inside spec scope, no invented business logic. Integration
tests must be *proven able to fail* (temporarily break the feature → test goes
red); a green test that cannot fail proves nothing. TODOs may only point at a
future spec (`TODO(spec-009)`), never back at finished work.

## `[INVENTED]` marker discipline

Wherever the upstream source docs are silent, the decision is marked
`[INVENTED-n]` in the spec **with the reason**, before the code exists. An
invention without a written rationale is indistinguishable from a mistake six
months later. Do not retroactively relabel shipped behavior as spec'd.

## `docs/` is never pushed

The entire `docs/` tree is gitignored on purpose: analysis notes, upstream
reference clones (some carry their own `.git` — committing them would store a
gitlink and leave fresh clones with empty directories), and local working state.
Code comments still cite `docs/...` paths; they resolve only in a local checkout.
Never commit, publish, or restructure `docs/` as part of a code change, and never
treat a missing doc as a broken build.

## Frontend specifics (Vue 3)

- Prefer `.ts` declarations over `.d.ts`: `skipLibCheck` silently widens
  unresolved `.d.ts` names to `any`, which is how broken wire signatures ship
  past typecheck. Project-owned types live in checked `.ts` files.
- Pinia stores hold state, not documents: CodeMirror keeps one `EditorView`
  (`shallowRef` + `markRaw`), one `EditorState` per tab in a plain `Map`.
- Security boundaries in chat rendering are layered and none replaces another:
  `html:false` → DOMPurify allow-list → URL-scheme hook. Test sanitizers by
  parsing the DOM and asserting attributes, never by substring-matching escaped
  text (`onerror` survives escaping as inert text).

## Verify scripts own nothing they didn't create

Every `scripts/verify-spec-*.mjs` runs against a disposable tempdir data dir
(`SPEC_ADE_DATA_DIR`), registers temp fixture projects, kills only processes it
spawned itself, uses its own port (002→007 = 7392…7397), and cleans up in
`finally`. Run them from the repo root. A verifier that mutates real user state
is a bug in the verifier.
