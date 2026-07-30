# Spec ADE Desktop (Tauri v2) — Skeleton

This is a **skeleton only**. Full Tauri scaffolding (Rust `src-tauri/`, icons,
capabilities) is deferred to **Pha 9**.

## Sidecar pattern (the core idea)

The desktop app does **not** reimplement the backend. It ships the compiled
`spec-ade-server` binary as a **Tauri sidecar** and points a WebView at it:

```
Tauri shell (WebView)  ──http://127.0.0.1:<port>──▶  spec-ade-server (sidecar)
        │                                                   │
        └── tauri-plugin-shell: Command::new_sidecar() ─────┘
```

Key points:

- **`bundle.externalBin`** lists the server binary. Tauri requires a
  **target-triple suffix** on the file name
  (e.g. `spec-ade-server-x86_64-apple-darwin`), or `tauri build` won't find it.
- Launch the sidecar via **`tauri-plugin-shell`** `Command::new_sidecar(...)`,
  then load `http://127.0.0.1:<port>` in the WebView.
- **Dynamic port:** the server binds `:0` and reports the chosen port back, so
  parallel instances don't collide.
- **macOS release blocker:** the sidecar binary must be **code-signed +
  notarized** or Gatekeeper kills it. Budget time for this in Pha 9.

## What still needs scaffolding (Pha 9)

```
// TODO(phase-9): src-tauri/           — Rust crate (tauri + tauri-build 2.x)
// TODO(phase-9):   ├─ Cargo.toml       — tauri deps, build script
// TODO(phase-9):   ├─ build.rs         — tauri_build::build()
// TODO(phase-9):   ├─ src/main.rs      — spawn sidecar, read port, open WebView
// TODO(phase-9):   └─ capabilities/    — shell:allow-execute for the sidecar
// TODO(phase-9): icons/                — app icons per platform
// TODO(phase-9): build the SPA (../web) into the binary before bundling
```

`@tauri-apps/cli` **2.11.4** is the pinned CLI. `tauri.conf.json` in this directory
is a **stub** showing the `externalBin` placeholder only.
