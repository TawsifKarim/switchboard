import { invoke } from "@tauri-apps/api/core";

export type AppEntry = {
  id: string;
  name: string;
  directory: string;
  command: string;
  tag: string;
};

export const listApps = () => invoke<AppEntry[]>("list_apps");

export const addApp = (
  name: string,
  directory: string,
  command: string,
  tag: string,
) => invoke<AppEntry>("add_app", { name, directory, command, tag });

export const deleteApp = (id: string) => invoke<void>("delete_app", { id });

export type StatusSnapshot = {
  running: boolean;
  pid: number | null;
  last_exit: number | null;
};

export const startApp = (id: string) => invoke<number>("start_app", { id });
export const stopApp = (id: string) => invoke<void>("stop_app", { id });
export const getStatus = (id: string) =>
  invoke<StatusSnapshot>("get_status", { id });

export type ExitEvent = { id: string; code: number };
