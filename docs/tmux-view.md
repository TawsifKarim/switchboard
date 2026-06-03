# Tmux View — Plan

Status: design agreed, not yet implemented. This doc captures a brainstorm between the user and Claude so any fresh session can pick it up.

## Problem

Sometimes you need to watch two (or more) service terminals side by side at the same time. Today Switchboard's main view shows one focused terminal at a time. The "tmux view" gives you a multi-pane workspace where each pane mirrors the live output of any running app.

## Where it lives in the UI

- A new **"Tmux View"** button is added to the header **before the "Start All" button**.
- Clicking it **replaces the main dashboard area** with the tmux workspace.
- The same button toggles back to the normal dashboard (label flips, e.g. `Tmux View ⇄ Dashboard`).
- State (tabs, layouts, pane→app assignments, divider positions, tab names) **persists across reloads**.

## Core model

- The workspace is a set of **tabs**. Unlimited (soft cap — trust the user).
- Each tab has one of **5 layouts**, chosen at creation:
  1. **2-pane** — split vertical or horizontal
  2. **3-pane** — one wide row + two cells (top-row-of-2 + bottom-wide, or vice versa; or 3 columns)
  3. **4-pane** — 2×2 grid
  4. **5-pane** — half + half, one half holds a single pane, the other holds 4 (orientation choosable)
  5. **6-pane** — 3×2 or 2×3 grid (orientation choosable)
- For layouts where orientation matters (5 and 6, also the 2-pane split direction), the user picks the **variant when creating the tab**.
- After creation, the layout is fixed but **dividers between panes are draggable to resize**.
- No tmux-style "zoom one pane to fullscreen" — keep it simple, dividers are enough.

## Panes

- Panes are **read-only output mirrors** of already-spawned apps managed by `ProcessManager`. No typing into a pane.
- A fresh pane is empty and shows a placeholder ("Click to assign a terminal").
- Clicking an empty pane opens a **modal listing every app** in the dashboard. Only **running** apps are selectable; stopped ones are visible but disabled. No search field — the user sees the whole list.
- The **same app can be mirrored in multiple panes**, even across tabs. Each mirror scrolls independently.
- When an app is assigned to a pane, the pane **replays the app's full ring-buffer backlog** (the existing 300-line buffer from `process.rs`), then streams live output via the existing Tauri broadcast.
- Each pane has a **header** showing:
  - app name
  - working directory (dir name)
  - git branch
  - status dot (running/stopped)
  - "change terminal" icon (reopens the picker modal)
  - "close pane" icon (clears the assignment, pane goes empty again)
- Panes can be **reassigned by clicking the header**, and **swapped by drag-and-drop** between each other within a tab.
- If an assigned app **stops/crashes**, the pane keeps showing the last output with a **"stopped" badge**. If the user later restarts the app, the pane resumes streaming (bound by app ID).

All header data (cwd, git branch) is already tracked per app in the dashboard — surface it, don't re-derive.

## Tabs

- **Browser-style tab bar at the top** of the tmux workspace, with a `+` button at the end.
- Clicking `+` opens the **layout picker modal**: 5 cards with **mini SVG/CSS diagrams** of each grid. For 5- and 6-pane cards, the variants (e.g. "4-on-top vs 4-on-bottom", "3×2 vs 2×3") are sub-options on the same screen.
- New tabs are **auto-named** `Tab 1`, `Tab 2`, … . **Double-click** the tab label to rename.
- **Closing a tab** uses a **confirmation dialog** ("Close this tab?").

## Persistence

Everything tmux-view-related survives reloads:

- list of tabs (id, name, layout, variant)
- per-pane assignments (app id or empty)
- divider positions per tab
- currently active tab

Per the house rules: **flat JSON, atomic write** (temp → fsync → rename → fsync parent dir). New fields on existing structs use `#[serde(default, skip_serializing_if = "Option::is_none")]`. **No database.**

Open question for the implementer: should this live in the existing config file (extending `config.rs`) or a separate `tmux-view.json` next to it? Default suggestion: a separate file, since the schemas are independent and a corrupt tmux layout shouldn't risk the app registry.

## Streaming / mirroring

- Mirrors **reuse the existing per-app broadcast channel** in `ProcessManager`. No new PTY work — a pane is just another subscriber.
- The ring-buffer replay-on-attach behavior is already implemented; panes piggyback on it the same way the main terminal view does.
- Each pane owns its own **xterm.js instance** and `@xterm/addon-fit`. Resize on divider drag re-fits.
- Independent scroll per pane (each xterm has its own scrollback view of the same source).

## Out of scope (deliberately)

- No interactive typing in panes.
- No pane zoom/maximize.
- No splitting an existing pane further (the 5 layouts are the only shapes).
- No search in the picker modal.
- No multi-monitor / popout — replaces the dashboard area in the same window.

## House-rule check

- ✅ Schema additions additive + defaulted (rule 5)
- ✅ Atomic writes (rule 6)
- ✅ No edit modals for apps — picker modal is *selection*, not editing (rule 8)
- ✅ No database (rule 9)
- ✅ Push, not poll — pane streaming uses existing Tauri events (rule 4)
- ✅ State per concern: tmux workspace state belongs in a new store (e.g. `tmux.svelte.ts`), broadcasts still come from `ProcessManager` (rule 3)

## Suggested implementation order

1. **Schema + persistence.** Define `TmuxWorkspace { tabs: Vec<TmuxTab> }`, `TmuxTab { id, name, layout, variant, panes: Vec<Option<AppId>>, dividers }`. Atomic load/save. Commands: `tmux_load`, `tmux_save`.
2. **Frontend store.** `tmux.svelte.ts` mirrors the workspace, exposes `addTab`, `closeTab`, `renameTab`, `assignPane`, `clearPane`, `swapPanes`, `setDivider`. Debounced save on mutation.
3. **Toggle button + route.** Header button in `+page.svelte` swaps the main area between dashboard and `<TmuxWorkspace />`.
4. **Tab bar + layout-picker modal** with SVG previews of the 5 layouts and their variants.
5. **Tab renderer.** A `<TmuxTab />` component that lays out N panes per the chosen layout/variant using CSS grid + resizable dividers (reuse the existing resizable-split pattern from `+page.svelte`).
6. **Pane component.** Header (name/dir/branch/status + change/close icons), empty-state click → picker modal, xterm instance subscribed to the app's broadcast, replay of ring buffer on mount.
7. **Drag-to-swap** panes within a tab (likely reuse `svelte-dnd-action`, mind the `flushSync` pattern noted in CLAUDE.md).
8. **Stopped-app badge** + auto-resume when the app restarts (already free if subscription is bound to app id).
9. **Confirm-on-close-tab** dialog.
10. **Manual click-test pass** (rule 10 — frontend is verified by clicking).

## Decisions log (from the interview)

| Question | Decision |
| --- | --- |
| Where does tmux view live? | Replaces the main dashboard area; same button toggles back |
| Terminal source for panes? | Already-running apps managed by `ProcessManager` |
| Interactivity? | Read-only mirrors, no typing |
| Same app in multiple panes? | Allowed, duplicates fine, independent scroll |
| Persist tabs/layouts? | Yes, everything persists across reloads |
| Layout variants? | Fixed variants chosen at creation + draggable dividers |
| Pane picker UX? | Modal listing all apps; only running ones are selectable; no search |
| Tab bar style? | Browser-style top tab bar with `+` button |
| Reassign / swap panes? | Click header to reopen picker; drag-and-drop to swap |
| Layout picked when? | At tab creation, via modal with SVG previews |
| Tab names? | Auto `Tab N`, double-click to rename |
| Stopped app behavior? | Keep last output + "stopped" badge; auto-resume if restarted |
| Scale cap? | Soft / unlimited |
| Backlog on assign? | Full ring-buffer replay |
| Pane zoom? | No |
| Pane header content? | Name + cwd + git branch + status + change/close icons |
| Close-tab UX? | Confirmation dialog |
| Header data source? | Already tracked per app, just surface it |
| Layout picker visuals? | Mini SVG diagrams + variant sub-options for 5/6-pane |
