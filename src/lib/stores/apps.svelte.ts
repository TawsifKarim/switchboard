import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  addApp,
  deleteApp,
  listApps,
  reorderApps,
  startApp,
  stopApp,
  type AppEntry,
  type ExitEvent,
} from "$lib/ipc";

export type RuntimeStatus = "stopped" | "starting" | "running" | "stopping";

export type RuntimeState = {
  status: RuntimeStatus;
  pid: number | null;
  exitCode: number | null;
};

const DEFAULT_RUNTIME: RuntimeState = {
  status: "stopped",
  pid: null,
  exitCode: null,
};

class AppsStore {
  apps = $state<AppEntry[]>([]);
  runtime = $state<Record<string, RuntimeState>>({});
  loaded = $state(false);
  focusedId = $state<string | null>(null);

  private unlisten: UnlistenFn | null = null;
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
  ): Promise<AppEntry> {
    const entry = await addApp(name, directory, command, tag, port);
    await this.refresh();
    return entry;
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
    this.runtime[id] = { status: "starting", pid: null, exitCode: null };
  }

  private setStopping(id: string): void {
    const prev = this.runtime[id];
    this.runtime[id] = {
      status: "stopping",
      pid: prev?.pid ?? null,
      exitCode: null,
    };
  }

  private setRunning(id: string, pid: number): void {
    this.runtime[id] = { status: "running", pid, exitCode: null };
  }

  private setStopped(id: string, exitCode: number | null): void {
    const prev = this.runtime[id];
    this.runtime[id] = {
      status: "stopped",
      pid: prev?.pid ?? null,
      exitCode,
    };
  }
}

export const apps = new AppsStore();
