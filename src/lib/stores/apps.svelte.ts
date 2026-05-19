import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  addApp,
  deleteApp,
  listApps,
  startApp,
  stopApp,
  type AppEntry,
  type ExitEvent,
} from "$lib/ipc";

export type RuntimeStatus = "stopped" | "starting" | "running";

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

  async add(name: string, directory: string, command: string, tag: string): Promise<AppEntry> {
    const entry = await addApp(name, directory, command, tag);
    await this.refresh();
    return entry;
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
    // The exit listener handles state cleanup once the child actually exits.
    await stopApp(id);
  }

  private setStarting(id: string): void {
    this.runtime[id] = { status: "starting", pid: null, exitCode: null };
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
