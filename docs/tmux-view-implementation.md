# Tmux View — Implementation Checklist

Phased, ordered task list for building the Tmux View feature. Work top-to-bottom.
Each phase ends in something you can run and visually confirm. Tick boxes as you go.

Read `docs/tmux-view.md` first for the design, decisions log, and house-rule
alignment. Read `docs/PLAN.md` for project-wide rules.

## How to use this doc

Same rules as `docs/implementation.md`:

- Work **one phase at a time**. Do not start phase N+1 until every checkbox in
  phase N is ticked **and** every line in that phase's **Success criteria** is
  observably true.
- A phase is "done" only when each success criterion can be demonstrated
  (command output, screenshot, file contents). If you cannot demonstrate a
  criterion, stop and report the blocker instead of marking it complete.
- If a criterion cannot be satisfied as written, stop and ask. Do not weaken
  the criterion, do not skip ahead, and do not invent workarounds that change
  decisions recorded in `docs/tmux-view.md`.

---

## Phase 0 — Schema + persistence (Rust)

Goal: a `tmux-view.json` file alongside `apps.json` that can round-trip a
workspace.

- [ ] 0.1 Add a new module `src-tauri/src/tmux.rs` and `mod tmux;` it from
      `lib.rs`.
- [ ] 0.2 Define types with serde derives:
  - `enum Layout { Two, Three, Four, Five, Six }`
  - `enum Variant { … per layout, e.g. TwoVertical, TwoHorizontal, FiveTopHeavy, FiveBottomHeavy, SixThreeByTwo, SixTwoByThree, ThreeColumns, ThreeTopWide, ThreeBottomWide }`
  - `struct TmuxPane { app_id: Option<String> }` (None = empty pane)
  - `struct TmuxTab { id: String (ULID), name: String, layout: Layout, variant: Variant, panes: Vec<TmuxPane>, dividers: Vec<f32> }`
  - `struct TmuxWorkspace { version: u32, tabs: Vec<TmuxTab>, active_tab_id: Option<String> }`
- [ ] 0.3 All new fields on existing types use
      `#[serde(default, skip_serializing_if = "Option::is_none")]` where
      applicable (house rule 5).
- [ ] 0.4 Implement `tmux_path(app: &AppHandle) -> PathBuf` mirroring
      `config::config_path` — dev: project root, prod: config dir.
- [ ] 0.5 Implement `load() -> TmuxWorkspace` (returns default empty workspace
      if file missing) and `save(&TmuxWorkspace)` with the same **atomic write
      pattern** as `config.rs` (temp → fsync → rename → fsync parent dir).
      House rule 6.
- [ ] 0.6 Unit tests in `tmux.rs`:
  - default workspace round-trips through save+load
  - a workspace with 3 tabs, mixed layouts, some empty panes round-trips
  - loading a missing file yields the default

**Success criteria**
- `cargo test -p switchboard tmux` passes the three tests above.
- `cargo check` clean from `src-tauri/`.
- No new clippy warnings.

---

## Phase 1 — Tauri commands

Goal: frontend can load/save the workspace.

- [ ] 1.1 In `commands.rs`, add:
  - `tmux_load() -> Result<TmuxWorkspace, String>`
  - `tmux_save(workspace: TmuxWorkspace) -> Result<(), String>`
- [ ] 1.2 Register both in the `invoke_handler` in `lib.rs`.
- [ ] 1.3 Errors stringify at the boundary (house rule 12).
- [ ] 1.4 Add typed wrappers in `src/lib/ipc.ts`:
  `loadTmux(): Promise<TmuxWorkspace>` and
  `saveTmux(w: TmuxWorkspace): Promise<void>`, with matching TS types.

**Success criteria**
- From the dev window's devtools, `await invoke('tmux_load')` returns the
  default workspace `{ version: 1, tabs: [], active_tab_id: null }`.
- `await invoke('tmux_save', { workspace: <minimal valid value> })` writes a
  file at the expected `tmux-view.json` path (verify on disk).
- TS types compile (`pnpm check` clean).

---

## Phase 2 — Frontend store

Goal: a Svelte 5 store that owns the tmux workspace, debounced-saves on
mutation, and exposes all the mutations the UI will need.

- [ ] 2.1 Create `src/lib/stores/tmux.svelte.ts` as a `$state` class
      (mirror the shape of `apps.svelte.ts`). House rule 3.
- [ ] 2.2 State: `tabs: TmuxTab[]`, `activeTabId: string | null`,
      derived `activeTab`.
- [ ] 2.3 Mutations:
  - `addTab(layout, variant, name?) -> tabId`
  - `closeTab(tabId)`
  - `renameTab(tabId, name)`
  - `setActiveTab(tabId)`
  - `assignPane(tabId, paneIndex, appId)`
  - `clearPane(tabId, paneIndex)`
  - `swapPanes(tabId, i, j)`
  - `setDivider(tabId, dividerIndex, value)`
- [ ] 2.4 Load on construction (`tmux_load`); debounced-save (~300ms) on every
      mutation via `tmux_save`.
- [ ] 2.5 Compute `panes: Vec<TmuxPane>` length from `(layout, variant)` so
      `addTab` always produces the right number of empty panes.

**Success criteria**
- `pnpm check` clean.
- From devtools, calling `tmuxStore.addTab('Four', '...')` then reloading the
  app shows the same tab present (persistence round-trip works end to end).
- Mutations do not trigger more than one save per ~300ms burst (verify by
  logging in `tmux_save`).

---

## Phase 3 — Toggle button + workspace shell

Goal: a header button swaps the main area between the existing dashboard and
an empty `<TmuxWorkspace />` placeholder.

- [ ] 3.1 Add a `Tmux View` button in the header of `src/routes/+page.svelte`,
      placed **before the "Start All" button**.
- [ ] 3.2 Add a `$state` toggle (`tmuxOpen`) and swap which subtree renders.
      The toggle label flips between `Tmux View` and `Dashboard`.
- [ ] 3.3 Persist `tmuxOpen` in localStorage so reloads keep the active view.
- [ ] 3.4 Create `src/lib/components/tmux/TmuxWorkspace.svelte` rendering only
      a placeholder ("Tmux workspace — no tabs yet") for now.

**Success criteria**
- Clicking the new header button replaces the dashboard area with the
  placeholder; clicking again restores the dashboard.
- Reloading while in tmux view leaves you in tmux view; reloading while in
  dashboard view leaves you in dashboard view.
- No console errors.

---

## Phase 4 — Tab bar + layout picker modal

Goal: user can create tabs (with layout + variant), rename them, switch
between them, and confirm-close them.

- [ ] 4.1 `TmuxTabBar.svelte`: browser-style horizontal tabs with names,
      active highlight, a `+` button at the end.
- [ ] 4.2 Click `+` opens `LayoutPickerDialog.svelte` (use shadcn `dialog`):
  - 5 cards, one per layout, each showing a **mini SVG diagram** of the grid.
  - Layouts with variants (2, 3, 5, 6) expose variant sub-options on the same
    card (radio-style chips beneath the diagram).
  - Confirm button calls `tmuxStore.addTab(...)` and sets it active.
- [ ] 4.3 **Double-click** a tab label → inline rename (input field, Enter to
      save, Esc to cancel).
- [ ] 4.4 Each tab has a small `x` icon; clicking it opens a shadcn
      `alert-dialog` confirmation ("Close this tab?") before calling
      `closeTab`.
- [ ] 4.5 Clicking a tab calls `setActiveTab`.

**Success criteria**
- Can create one tab of each layout/variant; each survives reload with the
  correct pane count and divider defaults.
- Rename persists across reload.
- Close-tab dialog appears and cancelling leaves the tab in place.
- No console errors.

---

## Phase 5 — Tab renderer (layouts + draggable dividers)

Goal: a `<TmuxTab />` component lays out N panes using CSS grid, with
draggable dividers that persist their positions.

- [ ] 5.1 `TmuxTab.svelte`: switch on `(layout, variant)` to produce a CSS
      grid template. Each cell renders a `<TmuxPane />` (empty for now).
- [ ] 5.2 Reuse the resizable-split pattern from `+page.svelte` for the
      divider handles. One handle per logical divider in the layout.
- [ ] 5.3 Dragging a divider calls `tmuxStore.setDivider(...)`. Default
      positions are 50% (or `1/N`) when a tab is freshly created.
- [ ] 5.4 Window resize keeps proportions (CSS-grid fractions handle this
      naturally).

**Success criteria**
- Each of the 5 layouts (and their variants) renders the correct number of
  cells in the correct shape.
- Dragging a divider visibly resizes panes; releasing then reloading restores
  the same proportions.
- No layout shift / overflow at small window sizes (panes get smaller, layout
  stays intact).

---

## Phase 6 — Pane component (empty state + picker modal)

Goal: empty panes show a placeholder, clicking opens a modal listing all
apps; only running apps are selectable; choosing one assigns it.

- [ ] 6.1 `TmuxPane.svelte` with two states: empty and assigned.
- [ ] 6.2 Empty state: centered button "Assign terminal". Clicking opens
      `AppPickerDialog.svelte`.
- [ ] 6.3 `AppPickerDialog.svelte`: lists every app from `apps.svelte.ts`. No
      search. Stopped apps are visible but disabled (and labeled "stopped").
      Selecting a running app calls `tmuxStore.assignPane(...)`.
- [ ] 6.4 Assigned state: shows the pane header (name + cwd dir name + git
      branch + status dot + change/close icons) — output rendering comes in
      Phase 7.

**Success criteria**
- Empty pane → picker modal → assign a running app → header reflects the
  assignment; reload preserves it.
- Stopped apps appear in the modal but cannot be selected.
- Clicking the header's "change" icon reopens the picker; "close" icon clears
  the pane back to empty.

---

## Phase 7 — Output mirroring (xterm + backlog replay)

Goal: assigned panes show live output with the existing ring-buffer replay,
each pane scrolling independently.

- [ ] 7.1 In `<TmuxPane />` assigned state, mount an xterm.js instance with
      `@xterm/addon-fit`, exactly the way the main `TerminalPanel` /
      `XtermView` does.
- [ ] 7.2 On mount, fetch the existing ring buffer for the app and write it
      into the pane's terminal.
- [ ] 7.3 Subscribe to the existing per-app Tauri broadcast event and
      `term.write(chunk)` on each message. **No new PTY work** — panes are
      just additional subscribers (house rule 4).
- [ ] 7.4 Disable input (`term.options.disableStdin = true`) — panes are
      read-only.
- [ ] 7.5 Refit on divider drag and window resize.
- [ ] 7.6 Unsubscribe + dispose terminal on unmount / reassignment.

**Success criteria**
- Open two panes mirroring the **same** running app: both show the same live
  output and each can scroll independently.
- A pane mounted after the app already produced output shows the replayed
  backlog, then continues with live output without duplication or gaps.
- No memory leak after creating/closing many panes (verify via devtools
  memory snapshot before/after a round of churn).

---

## Phase 8 — Drag-to-swap panes

Goal: within a tab, panes can be reordered by drag-and-drop.

- [ ] 8.1 Use `svelte-dnd-action` (already in the project). Mind the
      `flushSync` pattern noted in CLAUDE.md — radix transforms confuse
      geometry; use plain elements for drop zones.
- [ ] 8.2 Dragging pane A onto pane B calls `tmuxStore.swapPanes(tabId, i, j)`.
- [ ] 8.3 Visual feedback while dragging (drop target highlight).

**Success criteria**
- Swap visibly works for all 5 layouts.
- Swapping two assigned panes does not break their xterm instances (no flash
  of empty state, no duplicate replay). If swapping requires remounting, the
  replay+live-stream behavior from Phase 7 must still hold afterward.
- Swap persists across reload.

---

## Phase 9 — Stopped/restart behavior

Goal: an assigned app that stops keeps its last output with a clear badge; if
it restarts, the pane resumes streaming.

- [ ] 9.1 Pane subscribes to app status (already broadcast by
      `ProcessManager`) and shows a "stopped" badge in the header when the
      app is not running.
- [ ] 9.2 Last output remains visible (no clear-on-stop).
- [ ] 9.3 On restart, the subscription resumes — verify new output appears
      without remounting the pane.

**Success criteria**
- Start an app, assign it to a pane, stop the app: "stopped" badge appears,
  output stays visible.
- Restart the app: badge disappears, new output streams into the same pane.

---

## Phase 10 — Polish + manual QA

- [ ] 10.1 Empty workspace state: when there are no tabs, the workspace shows
      a "Create your first tab" prompt that opens the layout picker.
- [ ] 10.2 Keyboard: Esc closes any open dialog. Enter confirms rename.
- [ ] 10.3 Small-screen sanity: 6-pane layout at the default window size is
      still legible (or shows a soft warning if window is too small).
- [ ] 10.4 Click-test pass (house rule 10):
  - Create one tab of each of the 5 layouts (with each variant).
  - Assign 2 running apps to 4 different panes across 2 tabs.
  - Drag dividers, drag-swap panes, rename a tab, close a tab.
  - Reload the app; verify everything is exactly as left.
  - Stop one of the assigned apps; verify badge. Restart it; verify resume.
  - Toggle back to dashboard view and back to tmux view; state intact.
- [ ] 10.5 Update `docs/tmux-view.md` if any decision changed during build.

**Success criteria**
- Every item in 10.4 verified hands-on.
- No console errors or Rust panics during the click-test pass.
- `pnpm check` clean, `cargo check` + `cargo clippy` clean from `src-tauri/`.

---

## Out of scope (do not implement)

- Interactive typing in panes.
- Pane zoom / maximize.
- Splitting an existing pane further.
- Search in the picker modal.
- Popout window / multi-monitor.

If a future requirement pushes against any of these, update `docs/tmux-view.md`
first and get agreement before changing this checklist.
