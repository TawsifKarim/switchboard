import { invoke } from "@tauri-apps/api/core";

export type AppEntry = {
  id: string;
  name: string;
  directory: string;
  command: string;
  tag: string;
  port?: number | null;
};

export const listApps = () => invoke<AppEntry[]>("list_apps");

export const addApp = (
  name: string,
  directory: string,
  command: string,
  tag: string,
  port: number | null = null,
) => invoke<AppEntry>("add_app", { name, directory, command, tag, port });

export const deleteApp = (id: string) => invoke<void>("delete_app", { id });

export const reorderApps = (orderedIds: string[]) =>
  invoke<void>("reorder_apps", { orderedIds });

export type StatusSnapshot = {
  running: boolean;
  pid: number | null;
  last_exit: number | null;
};

export const startApp = (id: string) => invoke<number>("start_app", { id });
export const stopApp = (id: string) => invoke<void>("stop_app", { id });
export const getStatus = (id: string) =>
  invoke<StatusSnapshot>("get_status", { id });

export type StartAllResult = {
  started: string[];
  failed: [string, string][];
};
export const startAll = () => invoke<StartAllResult>("start_all");
export const stopAll = () => invoke<void>("stop_all");

export type ExitEvent = { id: string; code: number };

export type AppStats = { id: string; cpu_pct: number; rss_bytes: number };

export const getBranch = (directory: string) =>
  invoke<string | null>("get_branch", { directory });

export const openShell = (directory: string) =>
  invoke<string>("open_shell", { directory });

export const closeShell = (id: string) =>
  invoke<void>("close_shell", { id });
