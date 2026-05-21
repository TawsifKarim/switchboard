import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  addApp,
  deleteApp,
  getStatus,
  listApps,
  reorderApps,
  startAll,
  startApp,
  stopAll,
  stopApp,
  type AppEntry,
  type AppStats,
  type ExitEvent,
  type ReadyEvent,
  type ReadyProbe,
} from "$lib/ipc";

export type RuntimeStatus = "stopped" | "starting" | "running" | "stopping";

export type RuntimeState = {
  status: RuntimeStatus;
  pid: number | null;
  exitCode: number | null;
  ready: boolean;
};

const DEFAULT_RUNTIME: RuntimeState = {
  status: "stopped",
  pid: null,
  exitCode: null,
  ready: false,
};

class AppsStore {
  apps = $state<AppEntry[]>([]);
  runtime = $state<Record<string, RuntimeState>>({});
  stats = $state<Record<string, AppStats>>({});
  loaded = $state(false);
  focusedId = $state<string | null>(null);

  private unlisten: UnlistenFn | null = null;
  private unlistenStats: UnlistenFn | null = null;
  private unlistenReady: UnlistenFn | null = null;
  private initialized = false;

  focus(id: string | null): void {
    this.focusedId = id;
  }

  async init(): Promise<void> {
    if (this.initialized) return;
    this.initialized = true;
    await this.refresh();
    this.unlisten = await listen<ExitEvent>("app-exit", (e) => {
      this.setStopped(e.payload.id, e.payload.code);
      // The process is gone; don't leave stale CPU/RAM next to the PID.
      delete this.stats[e.payload.id];
    });
    this.unlistenStats = await listen<AppStats>("app-stats", (e) => {
      this.stats[e.payload.id] = e.payload;
    });
    this.unlistenReady = await listen<ReadyEvent>("app-ready", (e) => {
      const cur = this.runtime[e.payload.id];
      if (!cur) return;
      this.runtime[e.payload.id] = { ...cur, ready: e.payload.ready };
    });
  }

  async refresh(): Promise<void> {
    this.apps = await listApps();
    this.loaded = true;
  }

  runtimeOf(id: string): RuntimeState {
    return this.runtime[id] ?? DEFAULT_RUNTIME;
  }

  async add(
    name: string,
    directory: string,
    command: string,
    tag: string,
    port: number | null = null,
    ready: ReadyProbe | null = null,
    dependsOn: string[] = [],
  ): Promise<AppEntry> {
    const entry = await addApp(
      name,
      directory,
      command,
      tag,
      port,
      ready,
      dependsOn,
    );
    await this.refresh();
    return entry;
  }

  /** Most recent start_all outcome, surfaced for inline UI banners. */
  lastStartAll = $state<{
    started: number;
    failed: [string, string][];
    skipped: [string, string][];
  } | null>(null);

  dismissLastStartAll(): void {
    this.lastStartAll = null;
  }

  /** Live order update from dnd `consider` events — no backend call. */
  setOrder(items: AppEntry[]): void {
    this.apps = items;
  }

  /** Commit the new order to disk. On error, revert by re-fetching. */
  async reorder(orderedIds: string[]): Promise<void> {
    const byId = new Map(this.apps.map((a) => [a.id, a]));
    const next: AppEntry[] = [];
    for (const id of orderedIds) {
      const a = byId.get(id);
      if (a) next.push(a);
    }
    if (next.length === this.apps.length) this.apps = next;
    try {
      await reorderApps(orderedIds);
    } catch (e) {
      console.error("reorder failed; reverting", e);
      await this.refresh();
    }
  }

  async remove(id: string): Promise<void> {
    await deleteApp(id);
    delete this.runtime[id];
    delete this.stats[id];
    if (this.focusedId === id) this.focusedId = null;
    await this.refresh();
  }

  async start(id: string): Promise<void> {
    this.setStarting(id);
    try {
      const pid = await startApp(id);
      this.setRunning(id, pid);
    } catch (e) {
      this.setStopped(id, null);
      throw e;
    }
  }

  async startAll(): Promise<void> {
    // Clear any stale banner from a prior run before this attempt produces a new one.
    this.lastStartAll = null;
    // Mark every not-yet-running app as starting so the UI reflects intent
    // immediately; the backend skips ones already running.
    for (const a of this.apps) {
      const rt = this.runtime[a.id];
      if (!rt || rt.status === "stopped") this.setStarting(a.id);
    }
    try {
      const result = await startAll();
      // Pull the assigned PID for each newly-started app and flip to running.
      await Promise.all(
        result.started.map(async (id) => {
          try {
            const s = await getStatus(id);
            if (s.running && s.pid != null) this.setRunning(id, s.pid);
            else this.setStopped(id, s.last_exit);
          } catch (e) {
            console.error("getStatus after start_all failed", id, e);
          }
        }),
      );
      // Revert any "starting" markers for ids that didn't start.
      const startedSet = new Set(result.started);
      const failedSet = new Set(result.failed.map(([id]) => id));
      for (const a of this.apps) {
        const rt = this.runtime[a.id];
        if (
          rt?.status === "starting" &&
          !startedSet.has(a.id) &&
          !failedSet.has(a.id)
        ) {
          // Was already running on the backend side; sync from status.
          try {
            const s = await getStatus(a.id);
            if (s.running && s.pid != null) this.setRunning(a.id, s.pid);
            else this.setStopped(a.id, s.last_exit);
          } catch {
            this.setStopped(a.id, null);
          }
        }
      }
      for (const [id, err] of result.failed) {
        this.setStopped(id, null);
        console.warn("start_all: failed to start", id, err);
      }
      // Skipped apps were never started — revert their optimistic 'starting'
      // markers so the UI doesn't show them spinning forever.
      for (const [id] of result.skipped) {
        this.setStopped(id, null);
      }
      if (result.failed.length || result.skipped.length) {
        console.warn(
          `start_all: ${result.started.length} started, ${result.failed.length} failed, ${result.skipped.length} skipped`,
        );
      }
      this.lastStartAll = {
        started: result.started.length,
        failed: result.failed,
        skipped: result.skipped,
      };
    } catch (e) {
      console.error("start_all failed", e);
      // Best-effort revert of optimistic 'starting' markers.
      for (const a of this.apps) {
        if (this.runtime[a.id]?.status === "starting") {
          this.setStopped(a.id, null);
        }
      }
      throw e;
    }
  }

  async stopAll(): Promise<void> {
    // Mark every running/starting app as stopping; exit events flip to stopped.
    for (const a of this.apps) {
      const rt = this.runtime[a.id];
      if (rt?.status === "running" || rt?.status === "starting") {
        this.setStopping(a.id);
      }
    }
    try {
      await stopAll();
    } catch (e) {
      console.error("stop_all failed", e);
      throw e;
    }
  }

  async stop(id: string): Promise<void> {
    // Mark stopping so the row can show a "terminating…" spinner. The exit
    // listener flips to 'stopped' when the child actually exits (could be up
    // to 6s away if the process ignores SIGTERM + the port sweep needs its
    // 1s grace).
    this.setStopping(id);
    try {
      await stopApp(id);
    } catch (e) {
      // Surface the error but don't strand the row in 'stopping'.
      this.setStopped(id, null);
      throw e;
    }
  }

  private setStarting(id: string): void {
    // Reset ready on every start — the probe will flip it true if/when it
    // resolves. Avoids stale green after a restart.
    this.runtime[id] = { status: "starting", pid: null, exitCode: null, ready: false };
  }

  private setStopping(id: string): void {
    const prev = this.runtime[id];
    this.runtime[id] = {
      status: "stopping",
      pid: prev?.pid ?? null,
      exitCode: null,
      ready: prev?.ready ?? false,
    };
  }

  private setRunning(id: string, pid: number): void {
    const prev = this.runtime[id];
    this.runtime[id] = { status: "running", pid, exitCode: null, ready: prev?.ready ?? false };
  }

  private setStopped(id: string, exitCode: number | null): void {
    const prev = this.runtime[id];
    this.runtime[id] = {
      status: "stopped",
      pid: prev?.pid ?? null,
      exitCode,
      ready: false,
    };
  }
}

export const apps = new AppsStore();
