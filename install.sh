#!/usr/bin/env bash
#
# Switchboard installer.
# Checks/installs the build prerequisites, installs JS dependencies, builds the
# native macOS .app from source, and opens it.
#
# Building locally (rather than downloading a prebuilt binary) is deliberate:
# an app you compile on your own Mac isn't quarantined by Gatekeeper, so it
# launches with a normal double-click — no Apple Developer signing required.
#
# Usage:  ./install.sh
#
set -euo pipefail

# ---- pretty output -------------------------------------------------------
bold=$(printf '\033[1m'); dim=$(printf '\033[2m'); reset=$(printf '\033[0m')
blue=$(printf '\033[34m'); green=$(printf '\033[32m'); yellow=$(printf '\033[33m'); red=$(printf '\033[31m')
info() { printf '%s==>%s %s\n' "$blue$bold" "$reset" "$1"; }
ok()   { printf '%s ok %s %s\n' "$green$bold" "$reset" "$1"; }
warn() { printf '%s !! %s %s\n' "$yellow$bold" "$reset" "$1"; }
err()  { printf '%s xx %s %s\n' "$red$bold" "$reset" "$1" >&2; }

# Run from the repo root regardless of where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

printf '\n%sSwitchboard installer%s\n' "$bold" "$reset"
printf '%sBuilds the macOS app from source and opens it.%s\n\n' "$dim" "$reset"

# ---- 1. macOS only -------------------------------------------------------
if [ "$(uname)" != "Darwin" ]; then
  err "Switchboard is a macOS app. This installer only runs on macOS."
  exit 1
fi
ok "macOS detected"

# ---- 2. Xcode Command Line Tools (compiles the Rust/Tauri backend) -------
if ! xcode-select -p >/dev/null 2>&1; then
  warn "Xcode Command Line Tools are required to compile the app."
  info "Launching Apple's installer popup..."
  xcode-select --install >/dev/null 2>&1 || true
  err "Finish the Command Line Tools install in the popup, then re-run ./install.sh"
  exit 1
fi
ok "Xcode Command Line Tools present"

# ---- 3. Rust toolchain (cargo) via rustup --------------------------------
# cargo may be installed but not yet on PATH in this shell — source it first.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  info "Installing Rust via rustup (https://rustup.rs)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
ok "Rust $(rustc --version 2>/dev/null | awk '{print $2}')"

# ---- 4. Node.js 18+ ------------------------------------------------------
node_major() { node -v 2>/dev/null | sed 's/^v//; s/\..*//'; }
if ! command -v node >/dev/null 2>&1 || [ "$(node_major)" -lt 18 ] 2>/dev/null; then
  warn "Node.js 18+ is required."
  if command -v brew >/dev/null 2>&1; then
    info "Installing Node via Homebrew..."
    brew install node
  else
    err "Couldn't find Node 18+ and Homebrew isn't installed."
    err "Install Node 18+ from https://nodejs.org (or install Homebrew first), then re-run ./install.sh"
    exit 1
  fi
fi
ok "Node $(node -v)"

# ---- 5. pnpm (the project's package manager) -----------------------------
# Prefer Corepack (ships with Node) so the version matches the repo; fall back
# to a global npm install if Corepack isn't usable.
if ! command -v pnpm >/dev/null 2>&1; then
  info "Setting up pnpm..."
  if command -v corepack >/dev/null 2>&1; then
    corepack enable >/dev/null 2>&1 || true
    corepack prepare pnpm@latest --activate >/dev/null 2>&1 || true
  fi
  command -v pnpm >/dev/null 2>&1 || npm install -g pnpm
fi
ok "pnpm $(pnpm --version)"

# ---- 6. JS dependencies --------------------------------------------------
info "Installing JavaScript dependencies (pnpm install)..."
pnpm install

# ---- 7. Build the release .app -------------------------------------------
info "Building the macOS app (pnpm tauri build) — first build compiles Rust in release mode and can take a few minutes..."
pnpm tauri build

# ---- 8. Open it ----------------------------------------------------------
APP="$(/usr/bin/find src-tauri/target/release/bundle/macos -maxdepth 1 -name '*.app' -print -quit 2>/dev/null || true)"
if [ -n "$APP" ]; then
  ok "Built: $APP"
  info "Opening the app..."
  open "$APP"
  printf '\n%sDone.%s Drag %s%s%s into /Applications to keep it.\n\n' \
    "$green$bold" "$reset" "$bold" "$APP" "$reset"
else
  err "Build finished but the .app wasn't found under src-tauri/target/release/bundle/macos."
  err "Check the build output above for errors."
  exit 1
fi
