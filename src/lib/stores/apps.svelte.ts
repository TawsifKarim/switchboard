import { addApp, deleteApp, listApps, type AppEntry } from "$lib/ipc";

class AppsStore {
  apps = $state<AppEntry[]>([]);
  loaded = $state(false);

  async refresh(): Promise<void> {
    this.apps = await listApps();
    this.loaded = true;
  }

  async add(name: string, directory: string, command: string, tag: string): Promise<AppEntry> {
    const entry = await addApp(name, directory, command, tag);
    await this.refresh();
    return entry;
  }

  async remove(id: string): Promise<void> {
    await deleteApp(id);
    await this.refresh();
  }
}

export const apps = new AppsStore();
