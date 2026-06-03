# Switchboard

Switchboard is a control panel for your local microservices. When you're working on one service but need its dependencies running too, Switchboard keeps every process alive and gives you a single dashboard — start, stop, and watch the live terminal output of each service from one place, instead of juggling a dozen terminal tabs.

## Getting started

There are two ways to run Switchboard, depending on what you want:

### Build and run the app — `./install.sh`

Best when you want a real, double-clickable macOS app. From a fresh clone, the
installer checks/installs the prerequisites (Xcode Command Line Tools, Rust,
Node, pnpm), installs dependencies, builds the native `.app`, and opens it:

```sh
./install.sh
```

Because the app is compiled on your own Mac, it isn't quarantined by Gatekeeper
and launches with a normal double-click — no Apple Developer signing needed.

After pulling new changes, re-run `./install.sh` to rebuild the app with the
latest code (it re-syncs dependencies too).

### Run in dev mode (live reload)

Best while you're working on the code — the app hot-reloads as you edit:

```sh
pnpm install
pnpm tauri dev
```

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

`./install.sh` installs any of these that are missing, so you only need them if you build manually.

## Build the .app manually

`./install.sh` does this for you, but you can also build directly:

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
