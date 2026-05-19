# Switchboard

Local dev launcher for your microservices.

## What it does

- Register an app with a name, working directory, and a command (e.g. `air`).
- Toggle each app on or off with a switch — Switchboard spawns it under your login shell so PATH, asdf/mise, Homebrew, etc. all resolve normally.
- View live, colored terminal output for the focused app in the right panel.
- Lives in the macOS menu bar — closing the window keeps your services running; quit from the tray to stop everything cleanly.

## Prerequisites

- macOS
- Node 18+
- pnpm
- Rust 1.75+ (install via [rustup](https://rustup.rs))

## Setup

```sh
pnpm install
```

## Run in dev

```sh
pnpm tauri dev
```

## Build .app

```sh
pnpm tauri build
```

The bundle lands under `src-tauri/target/release/bundle/macos/switchboard.app` and a `.dmg` is produced alongside it.

## Config location

- **Dev (`pnpm tauri dev`):** `./apps.json` at the project root, with logs in `./logs/`.
- **Release (`.app`):** `~/.config/switchboard/apps.json`, with logs in `~/.config/switchboard/logs/`.

The config file is a single JSON document (no database). It's safe to hand-edit when the app is not running.

## Architecture

Tauri 2 shell (Rust backend + SvelteKit + shadcn-svelte frontend), `portable-pty` for real PTYs, broadcast channels for fan-out from the PTY reader to the xterm.js panel and the per-app log file. See [`docs/PLAN.md`](docs/PLAN.md) for the full design, decisions, and house rules; [`docs/implementation.md`](docs/implementation.md) for the phased build log.
