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
