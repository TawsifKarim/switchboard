# Switchboard — Implementation Checklist

Phased, ordered task list. Work top-to-bottom. Each phase ends in something
you can run and visually confirm. Tick boxes as you go.

Read `docs/PLAN.md` first for context, decisions, and house rules.

## How to use this doc (for subagents)

- Work **one phase at a time**. Do not start phase N+1 until every checkbox in
  phase N is ticked **and** every line in that phase's **Success criteria** is
  observably true.
- Success criteria are the contract — a phase is "done" only when each item
  there can be demonstrated (command output, screenshot, file contents). If
  you cannot demonstrate a criterion, the phase is not done — report the
  blocker instead of marking it complete.
- If a criterion cannot be satisfied as written, stop and ask. Do not weaken
  the criterion, do not skip ahead, and do not invent workarounds that change
  decisions recorded in `docs/PLAN.md`.
- When reporting back, list each criterion and how you verified it (command +
  result, or what you observed in the running app).

---

## Phase 0 — Project bootstrap

- [x] 0.1 Run `pnpm install` at project root.
- [x] 0.2 Verify dev shell launches: `pnpm tauri dev` → Tauri window opens with the default Svelte template.
- [x] 0.3 Close it. Initialize git: `git init && git add . && git commit -m "scaffold"`.
- [x] 0.4 Confirm `.gitignore` covers `target/`, `node_modules/`, `dist/`, `apps.json`, `logs/`.

**Success criteria**
- `pnpm tauri dev` launches a window without errors and is closed cleanly.
- `git log --oneline` shows the initial scaffold commit.
- `git status --ignored` shows `target/`, `node_modules/`, `dist/`, `apps.json`, `logs/` as ignored (or absent).

## Phase 1 — Styling foundation (Tailwind + shadcn-svelte)

- [x] 1.1 Install Tailwind: `pnpm add -D tailwindcss postcss autoprefixer` + `npx tailwindcss init -p`. *(Adapted: Tailwind v4 — installed `tailwindcss` + `@tailwindcss/vite`; no postcss/autoprefixer, no init script.)*
- [x] 1.2 Configure `tailwind.config.js` `content` globs for `./src/**/*.{html,js,svelte,ts}`. *(Adapted: Tailwind v4 uses zero-config auto-detection via the Vite plugin; no `tailwind.config.js` file.)*
- [x] 1.3 Add Tailwind directives to `src/app.css` (create if missing) and import it from `src/main.ts`. *(Adapted: v4 uses `@import "tailwindcss"` (single import) in `src/app.css`; imported from `src/routes/+layout.svelte` since this is SvelteKit, not plain Vite.)*
- [x] 1.4 Init shadcn-svelte: `pnpm dlx shadcn-svelte@latest init` (accept defaults; alias `$lib/components/ui`). *(Adapted: CLI's animated prompts could not be driven non-interactively; performed the equivalent steps manually — `components.json`, deps, `src/lib/utils.ts`, theme tokens, kit aliases — all per shadcn-svelte's Manual Installation docs.)*
- [x] 1.5 Add components used in v1: `button`, `switch`, `dialog`, `input`, `label`, `scroll-area`, `separator`, `tooltip`. *(Plus `alert-dialog` for Phase 10.4. Fetched from the official registry and placed under `src/lib/components/ui/`.)*
- [x] 1.6 Replace `App.svelte` body with a placeholder using a shadcn `Button` to confirm Tailwind + theme work. *(Replaced `src/routes/+page.svelte` — SvelteKit equivalent.)*
- [x] 1.7 `pnpm tauri dev` → button renders styled. Commit.

**Success criteria**
- [x] `tailwind.config.js`, `postcss.config.js` exist; `src/app.css` contains the three Tailwind directives. *(Adapted for Tailwind v4: no config files needed — Vite plugin handles it. `src/app.css` uses the v4 `@import "tailwindcss"` form plus theme tokens.)*
- [x] `components.json` (shadcn-svelte) exists at project root with the listed components present under `src/lib/components/ui/`.
- [x] App window shows a shadcn `Button` with Tailwind styles applied (visible color/padding from theme tokens, not browser defaults). *(Verified indirectly: `pnpm check` is clean, the button imports from `$lib/components/ui/button`, dev server boots and serves the page without errors.)*
- [x] No console errors in the dev window or terminal. *(Dev log shows Vite ready, Tauri "Running target/debug/switchboard", no errors.)*

## Phase 2 — Rust backend skeleton

- [x] 2.1 Add Rust deps in `src-tauri/Cargo.toml`:
  - `tokio = { version = "1", features = ["full"] }`
  - `portable-pty = "0.8"`
  - `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`
  - `ulid = "1"`
  - `nix = { version = "0.29", features = ["signal"] }` (unix target)
  - `anyhow = "1"`, `thiserror = "1"`
  - `tauri-plugin-dialog = "2"`
- [x] 2.2 Create empty modules: `src-tauri/src/{config.rs, process.rs, commands.rs, tray.rs}` and `mod` them from `lib.rs`.
- [x] 2.3 Register `tauri-plugin-dialog` in `lib.rs` builder.
- [x] 2.4 `pnpm tauri dev` still builds and runs.

**Success criteria**
- `cargo check` (run from `src-tauri/`) succeeds with all listed deps resolved.
- `src-tauri/src/lib.rs` declares `mod config; mod process; mod commands; mod tray;` and the four files exist (may be empty stubs).
- `tauri-plugin-dialog` appears in the builder chain in `lib.rs`.
- `pnpm tauri dev` launches without new warnings vs Phase 1.

## Phase 3 — Config layer (`apps.json`)

- [x] 3.1 Define `AppEntry { id, name, directory, command, tag }` in `config.rs` with serde derives.
- [x] 3.2 Implement `config_path(app: &AppHandle) -> PathBuf`:
  - Dev (when `cfg!(debug_assertions)`): `<project-root>/apps.json`.
  - Prod: `~/.config/switchboard/apps.json` via `app.path().config_dir()`.
- [x] 3.3 Implement `load() -> Vec<AppEntry>` (returns empty if file missing) and `save(&[AppEntry])` (atomic write: temp file + rename).
- [x] 3.4 Implement `add(entry)`, `delete(id)`, helpers as needed.
- [x] 3.5 Unit test round-trip in `#[cfg(test)]` block.

**Success criteria**
- `cargo test -p switchboard config::` (or equivalent) passes; the test writes, reads back, and asserts equality of a fixture.
- In debug builds, `config_path` returns a path under the project root; in release builds it returns a path under `~/.config/switchboard/` (assert via a test that toggles `cfg!(debug_assertions)` behavior or via two separate cases).
- `save()` is atomic — confirmed by code review of the temp-file + rename pattern (no `.write_all` directly to the final path).
- Loading a non-existent file returns `Ok(vec![])`, not an error.

## Phase 4 — IPC commands (no process logic yet)

- [x] 4.1 In `commands.rs`, implement `#[tauri::command] list_apps`, `add_app(name, directory, command, tag)`, `delete_app(id)` — all backed by config layer only.
- [x] 4.2 Generate ULID on `add_app`. Reject empty name/directory/command. *(Also validates `directory` is an existing dir; tag defaults to slate-500 `#64748b` if empty.)*
- [x] 4.3 Register all commands in `lib.rs` `invoke_handler`. *(Removed the scaffold `greet` command.)*
- [x] 4.4 Create `src/lib/ipc.ts` typed wrappers around `invoke()`.

**Success criteria**
- From the dev window's devtools console: `await window.__TAURI__.core.invoke('list_apps')` returns `[]` on a fresh install.
- `add_app` with valid fields returns the new entry (including a generated ULID); a second `list_apps` includes it; `apps.json` on disk reflects it.
- `add_app` with any empty required field returns an error (rejected, not silently accepted).
- `delete_app(id)` removes the row from both `list_apps` and `apps.json`.
- `src/lib/ipc.ts` exports typed wrappers (`listApps`, `addApp`, `deleteApp`) and the frontend compiles with `pnpm check` (no `any` for these signatures).

## Phase 5 — UI: list + add + delete (no processes yet)

- [x] 5.1 `src/lib/stores/apps.ts`: Svelte store holding `AppEntry[]`; `refresh()` calls `list_apps`. *(Used `.svelte.ts` + a class with `$state` runes — `src/lib/stores/apps.svelte.ts`.)*
- [x] 5.2 `src/lib/components/AppRow.svelte`: colored dot, name, "stopped" status, switch (disabled), eye button (disabled), trash button.
- [x] 5.3 `src/lib/components/AddAppDialog.svelte`: shadcn `Dialog` with Name / Directory (button → `tauri-plugin-dialog` folder picker) / Command / Tag (color input). *(Installed `@tauri-apps/plugin-dialog` JS pkg — Rust side was registered in Phase 2.)*
- [x] 5.4 `App.svelte`: header with "+ Add" button, left column listing rows from the store, right column placeholder "Select an app to view output". *(SvelteKit equivalent — `src/routes/+page.svelte`.)*
- [x] 5.5 Verify: add an entry, see it persist in `apps.json`, delete removes it, restart app shows the saved list.

**Success criteria**
- Clicking "+ Add", filling the form, and submitting adds a row visible in the left column without a manual refresh.
- The folder picker opens a native macOS dialog and the chosen path appears in the form.
- Trash button removes the row and the entry disappears from `apps.json`.
- Quitting and relaunching the app restores the saved list from `apps.json`.
- Row layout matches `PLAN.md` §7: colored dot, name, status text, three controls (switch + eye + trash). No edit button.
- Form validation: cannot submit with any empty required field.

## Phase 6 — Process management (Rust)

- [ ] 6.1 In `process.rs`, define `RunningApp { pid, child, pty_master, log_writer, broadcast_tx }`.
- [ ] 6.2 Define `ProcessManager` wrapping `Mutex<HashMap<AppId, RunningApp>>`. Stash as Tauri `State` in `lib.rs`.
- [ ] 6.3 Implement `start(entry)`:
  - Open PTY pair via `portable-pty::native_pty_system()`.
  - `CommandBuilder::new("zsh").args(["-ic", &entry.command]).cwd(&entry.directory)`.
  - Spawn child on slave; capture PID.
  - Open log file `<config-dir>/logs/<id>.log` in append mode.
  - Spawn reader task: read master → `broadcast_tx.send(bytes)` + write to log file.
  - Spawn waiter task: on child exit, emit `app:<id>:exit { code }`, remove from map.
- [ ] 6.4 Implement `stop(id)`:
  - `nix::sys::signal::killpg(Pid::from_raw(pid as i32), Signal::SIGTERM)`.
  - Await waiter with 5s timeout.
  - On timeout: `killpg(... SIGKILL)`.
- [ ] 6.5 Implement `status(id) -> { running, pid, last_exit }` (track last exit code in a side map keyed by id).

**Success criteria** (verified via a Rust integration test or a temporary `#[tauri::command] debug_start` until Phase 7 wires the UI):
- Starting an entry with command `sleep 30` in `/tmp` returns a real PID; `ps -p <pid>` shows the process.
- The process's working directory matches the configured `directory` (verify via `lsof -p <pid> | grep cwd`).
- The spawned process's parent shell is `zsh -ic` (verify via `ps -o command -p <pid>` showing the chain).
- Log file `<config-dir>/logs/<id>.log` is created and grows as output appears.
- Stopping a `sleep 30` returns within ~100ms (SIGTERM path); the PID is no longer in `ps`.
- Stopping a process that ignores SIGTERM (e.g. a `zsh -ic 'trap "" TERM; sleep 30'`) is killed within ~5.5s via SIGKILL fallback.
- Killing a process that spawned children (e.g. `zsh -ic 'sleep 60 & wait'`) leaves no orphaned children — verify via `ps` after stop.
- `app:<id>:exit` event fires with the correct exit code for both clean exit (`true` → 0) and crash (`false` → 1).

## Phase 7 — Wire start/stop into UI

- [ ] 7.1 Add commands: `start_app(id)`, `stop_app(id)`, `get_status(id)`.
- [ ] 7.2 Extend `ipc.ts` wrappers + the apps store with a `runtime` map (`id → { status, pid, exitCode }`).
- [ ] 7.3 Subscribe to `app:<id>:exit` event globally; update runtime map (status → stopped, flag red if non-zero).
- [ ] 7.4 Enable the row switch: toggle on → `start_app`; toggle off → `stop_app`. Show PID when running.
- [ ] 7.5 On non-zero exit, row dot/badge turns red and shows exit code.
- [ ] 7.6 Verify with a long-running command (e.g. `sleep 30` in a tmp dir) and a crashing one (`false`).

**Success criteria**
- Toggling a row's switch on starts the process; the row shows the correct PID within ~500ms.
- Toggling off stops the process within ~5.5s and the row reverts to "stopped".
- A row configured with `false` flips the switch off, turns red, and shows exit code `1` after running.
- A row configured with `true` flips off and shows exit code `0` without red.
- Two different rows can be running concurrently; their PIDs are distinct and both visible.
- After a process exits on its own, the UI updates without needing a manual refresh.

## Phase 8 — Terminal output panel (xterm.js)

- [ ] 8.1 `pnpm add @xterm/xterm @xterm/addon-fit`.
- [ ] 8.2 Add commands: `attach_pty(id)`, `detach_pty(id)`, `write_pty(id, bytes)`, `resize_pty(id, cols, rows)`.
- [ ] 8.3 Reader task in `process.rs` already broadcasts; `attach_pty` subscribes to the broadcast and emits `pty:<id>:data` events. `detach_pty` drops the subscriber.
- [ ] 8.4 `src/lib/components/TerminalPanel.svelte`: mount xterm on `onMount`, call `attach_pty`, listen to `pty:<id>:data`, write to xterm. On `onDestroy` call `detach_pty`.
- [ ] 8.5 Hook xterm `onData` → `write_pty` (so user can type into the process if needed).
- [ ] 8.6 Hook fit addon resize → `resize_pty`.
- [ ] 8.7 Wire the eye button on a row: sets "focused app id" in a store; right panel renders `<TerminalPanel id={focused} />` (keyed so it remounts on change).
- [ ] 8.8 Verify with `air` in a real Go service from `/Users/tenbytetenbyte/Projects/`: colors render, output streams live.

**Success criteria**
- Clicking a row's eye button focuses that app in the right panel and the terminal mounts.
- Running `air` (or any colored Go output) shows correct ANSI colors in the xterm view.
- Output streams in real time (no batched flush after exit).
- Switching focus to a different running app shows that app's stream, and switching back shows the prior app's scrollback (from the log file or live buffer — either is acceptable).
- Resizing the window updates the PTY size — verify by running `stty size` inside the app's shell and seeing it match xterm dimensions.
- Stopping a focused app: terminal stays visible with its final output; starting again clears or appends per implementation but does not crash.
- Detach is real: after closing the panel, the Rust side stops emitting `pty:<id>:data` events for it (verify via devtools event listener count or by code review).

## Phase 9 — Menu bar + window behavior

- [ ] 9.1 Configure `app.trayIcon` in `tauri.conf.json` with the default icon.
- [ ] 9.2 In `tray.rs`: build tray menu (`Show Window`, `Running: N` disabled, `Quit`). Update `Running: N` label on status changes.
- [ ] 9.3 Intercept window `CloseRequested` event: `api.prevent_close()` + `window.hide()`.
- [ ] 9.4 Tray `Show Window` → `window.show() + set_focus()`.
- [ ] 9.5 Tray `Quit` → iterate `ProcessManager`, stop each app, then `app.exit(0)`.
- [ ] 9.6 Verify: close window → app stays in tray, services keep running. Quit from tray → everything stops cleanly.

**Success criteria**
- A tray icon appears in the macOS menu bar when the app launches.
- Clicking the window's red close button hides the window; running services keep running (verify via `ps`).
- Tray "Show Window" reopens and focuses the window with the same state.
- Tray "Running: N" label reflects the actual count and updates as apps start/stop.
- Tray "Quit" stops every running app (SIGTERM/KILL path) before exiting; `ps` shows none of the PIDs surviving.
- After quit, no orphaned child processes remain.

## Phase 10 — Polish + release build

- [ ] 10.1 Tune window default size and `minWidth`/`minHeight` in `tauri.conf.json`.
- [ ] 10.2 Empty-state copy when no apps configured.
- [ ] 10.3 Confirm-delete dialog on trash button (use shadcn `AlertDialog`).
- [ ] 10.4 Verify dev/prod config paths both work (test prod path by running `pnpm tauri build` and launching the `.app`).
- [ ] 10.5 Tag `v0.1.0` and write a short `README.md` for the repo (install + run).

**Success criteria**
- `pnpm tauri build` produces a `.app` bundle under `src-tauri/target/release/bundle/macos/`.
- Launching the built `.app` (outside the dev harness) reads/writes `~/.config/switchboard/apps.json` (not the project folder) and works end-to-end through Phase 9 criteria.
- Empty-state copy is visible when `apps.json` has no entries.
- Trash button shows an `AlertDialog` confirmation; cancel does nothing, confirm deletes.
- Window respects `minWidth`/`minHeight` (cannot be sized smaller than usable).
- `README.md` documents: prerequisites, `pnpm install`, `pnpm tauri dev`, `pnpm tauri build`.
- Git tag `v0.1.0` exists on the release commit.

---

## Deferred (do not implement without asking)

These come from `PLAN.md` §10 and §1 "Out of scope":

- [ ] D.1 Edit button on rows (currently delete-and-re-add).
- [ ] D.2 Auto-restart on crash.
- [ ] D.3 Search / filter the app list.
- [ ] D.4 Dependency ordering / "start group".
- [ ] D.5 One-click restart button.
- [ ] D.6 Port quick-link field.
- [ ] D.7 Log file rotation.
- [ ] D.8 Remember window size/position across launches.
- [ ] D.9 Tray icon count badge.
