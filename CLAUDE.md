# Switchboard

A macOS desktop app that replaces the ritual of opening 8 terminal tabs to run a fleet of local microservices. Built for solo devs maintaining a polyrepo (or monorepo) of services who want to start, stop, and watch them from one place.

## What pain it resolves

- **Tab sprawl.** No more 12 iTerm tabs, none labeled, each running one service.
- **Cold-start ritual.** One click instead of `cd ~/Projects/auth && air`, repeat ×N every morning.
- **Lost stdout.** Switching focus doesn't kill output — every service has a 300-line ring buffer that replays when you re-open its terminal.
- **Zombie ports.** A crashed service that didn't release its port is killed by an `lsof` sweep on start/stop.
- **Hidden resource cost.** Per-service CPU/RAM (tree-aggregated) so you notice the runaway before your fans do.
- **Context switch tax.** Tray badge + always-visible header totals mean you don't have to open the app to know the fleet is healthy.

## Tech stack

- **Runtime:** Tauri 2 (Rust backend, WebKit frontend) — single-binary native macOS app
- **Frontend:** SvelteKit + Svelte 5 runes (`$state`, `$derived`, `$effect`, `$props`)
- **UI kit:** shadcn-svelte (vega preset) + Tailwind v4 via `@tailwindcss/vite`
- **Terminal:** xterm.js + `@xterm/addon-fit`
- **PTY:** `portable-pty` — spawns `zsh -ic 'exec zsh -c "$CMD"'` (interactive zsh for aliases/PATH, `exec` inner layer restores signal handling)
- **Process tree:** `sysinfo` 0.39 (walks pid → children for CPU/RAM aggregation)
- **DnD:** `svelte-dnd-action` (drag-disabled-flipped-via-`flushSync` pattern, plain `<aside>` not ScrollArea — radix transforms confuse geometry)
- **IDs:** ULID (one-off shells prefixed `oneoff:` so the sampler/tray filter them out)
- **Persistence:** atomic JSON (temp file → fsync → rename → parent-dir fsync). **No database.**
- **Signals:** `nix` (SIGTERM → SIGKILL escalation via `killpg` on the whole process group)

## Directory structure

```
switchboard/
├── docs/
│   ├── PLAN.md              # source of truth — schema, commands, house rules (§11)
│   └── implementation.md    # phased checklist with per-phase success criteria
├── src/                     # SvelteKit frontend
│   ├── routes/+page.svelte  # main shell: header + resizable split + dndzone
│   ├── lib/
│   │   ├── components/      # AppRow, AddAppDialog, TerminalPanel, XtermView
│   │   ├── stores/apps.svelte.ts  # single $state class — apps, runtime, stats, focusedId
│   │   └── ipc.ts           # typed wrappers over invoke()
│   └── app.css              # Tailwind entry
└── src-tauri/
    ├── src/
    │   ├── lib.rs           # setup hook + command registration + tray wiring
    │   ├── commands.rs      # #[tauri::command] surface — thin, delegates to pm/config
    │   ├── process.rs       # ProcessManager: PTY lifecycle, ring buffer, broadcast, sampler
    │   ├── config.rs        # AppEntry, atomic save, reorder, version check
    │   └── tray.rs          # tray icon + "Running: N" label
    ├── capabilities/default.json
    └── tauri.conf.json
```

## Coding standards (solo-senior pragmatism)

These rules exist to keep the codebase *flexible*, not *clever*. Optimize for the change you didn't see coming.

1. **No premature abstraction.** Three similar call sites is fine. Wait for the fourth, or for a divergence the duplication can't absorb, before extracting.
2. **Boundaries are typed; internals are loose.** `commands.rs` and `ipc.ts` are the contract — keep them aligned. Inside `process.rs` or a Svelte component, don't invent newtypes for clarity-only.
3. **State lives in one place per concern.** Frontend: `apps.svelte.ts` owns all UI-observable runtime state. Backend: `ProcessManager.inner` is the single source of process truth. Don't shadow it in components.
4. **Tauri events are the only push channel.** No polling loops on the frontend. If you need fresh data, emit an event from Rust (`app-started`, `app-exit`, `app-stats`, future `app-ready`).
5. **Schema additions are additive and defaulted.** New `AppEntry` fields use `#[serde(default, skip_serializing_if = "Option::is_none")]` so old config files still load. Refuse unknown config *versions*, not unknown *fields*.
6. **Atomic writes only.** Never overwrite config in place. Pattern: temp file → fsync → rename → fsync parent dir.
7. **PTY commands stay `zsh -ic`.** Don't "optimize" to direct `exec` — aliases, `PATH`, and nvm shims break otherwise. The inner `exec zsh -c` layer is what makes SIGTERM work; don't remove it.
8. **No edit modals.** Delete + re-add. (House rule from `PLAN.md` §11.)
9. **No database.** Flat JSON or in-memory. If you find yourself wanting Postgres, you've misunderstood the product.
10. **Test the Rust, not the Svelte.** Backend logic gets unit tests (`config`, ring buffer, reorder). Frontend is verified by clicking — if you can't click it, it doesn't ship.
11. **Comments explain *why*, never *what*.** A non-obvious workaround, a load-bearing ordering, a surprising invariant — one line. Naming covers the rest.
12. **Errors stringify at the Tauri boundary.** Internal code can use typed errors or `anyhow`. At the `#[tauri::command]` layer, return `Result<_, String>` so the frontend can `toast(e)`.
13. **Tauri setup runs outside Tokio.** Use `tauri::async_runtime::spawn`, never raw `tokio::spawn`, in the setup hook — raw spawn panics with `cannot_unwind` at launch.
14. **Long-running background work goes through `ProcessManager`.** Don't spawn ad-hoc tokio tasks from commands. The manager owns lifecycles, broadcasts, and cleanup.

## Reading order for a fresh session

1. `docs/PLAN.md` — what the app is and what it deliberately isn't
2. `src-tauri/src/process.rs` — the heart; understand the ring buffer + broadcast fan-out
3. `src/lib/stores/apps.svelte.ts` — how the UI reacts to backend events
4. `src/routes/+page.svelte` — how header, split, dnd, and terminal are wired
