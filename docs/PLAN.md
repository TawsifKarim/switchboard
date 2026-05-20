# Switchboard — Plan

A local-dev launcher for Go (and any) microservices. Add a directory + command,
toggle a switch to start/stop, view live terminal output in a side panel.

This document is the source of truth for a fresh Claude session picking up the
project. Read it top-to-bottom before touching code.

---

## 1. Problem we are solving

Running multiple Go microservices locally currently means opening N VSCode /
terminal tabs, `cd`-ing into each repo, and running a blocking dev command
(e.g. `air`). Switchboard replaces that ritual with one window:

- A persistent list of "apps" (name + directory + command + tag).
- A switch per row to start/stop the process.
- A button per row to focus its live terminal output in the right panel.
- The PID of the running process is shown on the row.
- App lives in the macOS menu bar; closing the window does not stop services.

Out of scope for v1: search, dependency ordering, restart button, in-place
edit, auto-restart on crash, port quick-links.

---

## 2. Stack & top-level decisions

| Area              | Choice                                                                 |
| ----------------- | ---------------------------------------------------------------------- |
| Shell             | Tauri 2 (Rust backend + web frontend)                                  |
| Frontend          | Svelte + TypeScript + Vite                                             |
| UI kit            | shadcn-svelte (Svelte port of shadcn/ui) + Tailwind CSS                |
| Terminal view     | xterm.js                                                               |
| PTY (Rust side)   | `portable-pty` crate                                                   |
| Process model     | Each app runs as `zsh -ic '<command>'` inside a PTY, in app's cwd      |
| Stop signal       | SIGTERM to process group → 5s grace → SIGKILL                          |
| Config storage    | JSON file (no DB). Dev: project folder. Prod: `~/.config/switchboard/` |
| Log persistence   | Per-app log file: `<config-dir>/logs/<app-id>.log`                     |
| Window behavior   | Close hides window; menu-bar icon stays; Quit truly exits + stops all  |
| Editing entries   | Delete-and-re-add (no edit modal in v1)                                |
| On crash          | Toggle flips off, row turns red, exit code shown. No auto-restart.     |

### Why these choices (when not obvious)

- **`zsh -ic`** — needs the user's interactive shell so `air`, asdf/mise shims,
  and Homebrew PATH resolve identically to running the command by hand. Without
  this, `air` and similar tools are typically "not found".
- **PTY (not just piped stdout)** — Go dev tools (`air`, `go run` w/ color)
  detect TTY and emit ANSI. We want faithful colors and interactive prompts.
- **`portable-pty`** — battle-tested (used by WezTerm). Handles macOS quirks
  and process-group signaling cleanly.
- **No DB** — explicit user requirement. The whole config is a single JSON file.
- **No edit modal** — explicit user requirement. Rows have **only** three
  controls: toggle switch, delete, view-output. To change a command you delete
  and re-add. Do not add an edit button without asking.

---

## 3. Repository layout

```
switchboard/
├── docs/
│   └── PLAN.md                  ← this file
├── src/                         ← Svelte frontend
│   ├── App.svelte
│   ├── lib/                     ← components, stores, ipc wrappers
│   └── main.ts
├── src-tauri/                   ← Rust backend
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs
│   │   ├── config.rs            ← apps.json read/write
│   │   ├── process.rs           ← spawn/stop, PTY management, log file
│   │   ├── commands.rs          ← #[tauri::command] handlers
│   │   └── tray.rs              ← menu-bar icon + quit handling
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── capabilities/
├── static/
├── package.json
└── vite.config.js
```

---

## 4. Config schema (`apps.json`)

Location:
- Dev (`pnpm tauri dev`): `<project-root>/apps.json`
- Prod (.app bundle): `~/.config/switchboard/apps.json`

Logs always live in `<config-dir>/logs/<app-id>.log`.

```jsonc
{
  "version": 1,
  "apps": [
    {
      "id": "01HZ...",            // ULID, generated on add
      "name": "auth-service",     // display name, unique
      "directory": "/Users/me/Projects/auth",
      "command": "air",           // raw command string, runs under zsh -ic
      "tag": "#3b82f6",           // hex color for the row dot/badge
      "port": 8080                // optional; see §6 for sweep semantics
    }
  ]
}
```

`port` is optional and additive — pre-port `apps.json` files load fine
(schema version stays `1`). When omitted, the field is not serialized.

Runtime state (PID, status, last exit code) is **not** persisted — it is
in-memory in the Rust backend and re-derived on each launch (everything starts
"stopped"). This matches the user's "open app → toggle on → done" flow.

---

## 5. Tauri command surface (Rust ↔ Svelte IPC)

All commands live in `src-tauri/src/commands.rs`. The frontend wraps these in
`src/lib/ipc.ts`.

```rust
list_apps() -> Vec<AppEntry>                  // from apps.json
add_app(name, directory, command, tag)        // append + persist
delete_app(id)                                // stop if running, then remove
start_app(id) -> u32                          // returns PID
stop_app(id)                                  // SIGTERM → 5s → SIGKILL
get_status(id) -> { running, pid, exit_code } // poll for UI refresh
attach_pty(id)                                // begin streaming PTY bytes
detach_pty(id)                                // stop streaming (keep process)
resize_pty(id, cols, rows)                    // forwarded from xterm.js
write_pty(id, bytes)                          // user keystrokes into the PTY
```

Events emitted to the frontend (Tauri event bus):

- `pty:<id>:data` — raw PTY bytes (binary or base64) for xterm.js
- `app:<id>:exit` — `{ code: i32 }` when process exits
- `app:<id>:status` — coarse status changes (`starting | running | stopped`)
- `app-stats` — `{ id, cpu_pct, rss_bytes }` emitted every 2s while running
  (tree-aggregated across the root pid + all descendants)

---

## 6. Process management (Rust side)

Module: `src-tauri/src/process.rs`.

- Maintain `HashMap<AppId, RunningApp>` behind a `tokio::sync::Mutex`.
- `RunningApp` holds: PTY master, child handle, PID, log file writer,
  broadcast channel for PTY bytes (so multiple subscribers — UI + log file —
  read the same stream).
- Spawn:
  1. Open PTY pair via `portable-pty`.
  2. Build command: `CommandBuilder::new("zsh")` + args `["-ic", command]`,
     `cwd(directory)`.
  3. Spawn on the slave side; capture child.
  4. Spawn a reader task: read PTY master → fan out to (a) log file writer,
     (b) broadcast channel for UI subscribers.
  5. Spawn a waiter task: on child exit, emit `app:<id>:exit`, drop from map.
- Stop:
  1. `killpg(pid, SIGTERM)` (process group — important so `air`'s spawned
     `go run` children also die).
  2. Wait up to 5s for the waiter task to observe exit.
  3. If still alive, `killpg(pid, SIGKILL)`.

### Optional port sweep

When `AppEntry.port` is set, the start and stop paths also sweep that port:

- **Pre-flight (in `start`)**: before opening the PTY, find anything bound to
  the port (`lsof -nP -iTCP:<port> -sTCP:LISTEN -t` + UDP), SIGTERM, wait 1s,
  SIGKILL any survivors. Clears stale leftovers from a previous run that
  didn't shut down cleanly.
- **Post-stop (in `stop`)**: after the SIGTERM/SIGKILL path finishes, re-run
  the same sweep. Safety net for orphaned children the process-group walk
  missed but that are still holding the port.

`lsof` is required for the sweep; if it's absent, the sweep logs and
no-ops rather than failing the start/stop flow.

### Scrollback buffer

Each app has a server-side ring buffer of recent PTY output (cap: 300 lines
or 512KB, whichever trims first). The buffer:

- **Resets on every new `start(id)`** — a fresh run starts with an empty
  scrollback so the user never sees stale output from a prior run.
- **Survives process exit** — you can still scroll back the tail of a
  crashed app until you either restart it or delete the app entry.
- **Is replayed on `attach`** — when the frontend re-focuses an app, the
  current snapshot is emitted as a single `pty:<id>:data` event before the
  live forward loop starts. This makes UI focus changes lossless from the
  user's perspective.
- Is cleared when the app entry is deleted (`delete_app` calls `clear_ring`).

The frontend `TerminalPanel` registers its `listen("pty:<id>:data")` BEFORE
calling `attach_pty`, so the replay event is never missed.

---

## 7. UI layout (Svelte + shadcn-svelte)

```
┌──────────────────────────────────────────────────────────────┐
│ Switchboard                                          [+ Add] │
├───────────────────────────┬──────────────────────────────────┤
│ ● auth-service   PID 4231 │  ┌─ Terminal: auth-service ────┐ │
│   [switch] [eye] [trash]  │  │ xterm.js view              │ │
│ ● payments      stopped   │  │                            │ │
│   [switch] [eye] [trash]  │  │                            │ │
│ ● gateway       PID 4288  │  │                            │ │
│   [switch] [eye] [trash]  │  └────────────────────────────┘ │
└───────────────────────────┴──────────────────────────────────┘
```

- Left: scrollable app list. Row = colored dot (tag), name, PID or status,
  three controls.
- Right: one terminal at a time. Header shows which app is focused.
- `+ Add` opens a modal with: Name, Directory (folder picker), Command, Tag color.
- No edit button. To change something: trash → re-add.

State management: a single Svelte store for `apps[]` and `runtime{ id → {pid,
status, exitCode} }`. The store subscribes to Tauri events and refreshes on IPC
responses.

---

## 8. Menu-bar / window behavior

- Configure tray icon in `tauri.conf.json` (`app.trayIcon`) and the Rust setup
  hook (`src-tauri/src/tray.rs`).
- Tray menu: `Show Window`, `Running: N`, `Quit`.
- Window close button → hide window (intercept `CloseRequested`, call
  `window.hide()` instead of exiting).
- Tray `Quit` → iterate running apps, stop each (SIGTERM/KILL), then exit.

---

## 9. Current status

**Done**
- Decisions captured (this doc).
- Rust 1.95 installed via rustup.
- Tauri 2 + Svelte + TS scaffold created at project root via
  `pnpm create tauri-app`.
- `docs/PLAN.md` written.

**Not yet done — start here**
1. `pnpm install` in project root.
2. Add Tailwind CSS + shadcn-svelte to the Svelte project. Initialize the
   shadcn-svelte CLI and add the components we will use: `button`, `switch`,
   `dialog`, `input`, `label`, `scroll-area`, `separator`.
3. Add Rust deps to `src-tauri/Cargo.toml`:
   - `portable-pty`
   - `tokio` (with `full`)
   - `serde`, `serde_json`
   - `ulid`
   - `nix` (for `killpg`, signals) — unix only
   - `anyhow` or `thiserror` for error handling
   - `tauri-plugin-dialog` (folder picker for the Add modal)
4. Implement `src-tauri/src/config.rs`: read/write `apps.json` with the dev vs
   prod path logic from §4. Use `tauri::path::BaseDirectory::Config` in prod.
5. Implement `src-tauri/src/process.rs` per §6.
6. Wire `src-tauri/src/commands.rs` per §5 and register in `lib.rs`.
7. Build the Svelte UI per §7. Start with the list + add modal + start/stop.
   Add the xterm.js panel last.
8. Tray + menu-bar behavior per §8.
9. Verify end-to-end with a real Go service (`air` in one of the user's repos
   under `/Users/tenbytetenbyte/Projects/`).

**Run commands**
- Dev: `pnpm tauri dev`
- Build .app: `pnpm tauri build`

---

## 10. Open questions / things to confirm later

- Window dimensions and whether to remember size/position across launches.
- Whether the tray icon should show a count badge of running services.
- Log file rotation policy (currently: append forever; revisit if files grow).
- Whether to support a "stop all" shortcut/menu item.

These are deliberately deferred — do not implement without asking the user.

---

## 11. House rules for this codebase

- **Do not** add features beyond v1 scope (see §1 "Out of scope") without
  asking. Especially: no edit button, no auto-restart, no dependency graph.
- **Do not** introduce a database. Config is one JSON file.
- **Do not** swap `zsh -ic` for direct exec without discussing — it will break
  PATH for tools like `air`.
- Prefer editing existing files over creating new ones. Keep the module list
  in §3 small; add new modules only when a clear seam appears.
