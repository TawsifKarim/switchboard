<script lang="ts">
  import { Button } from "$lib/components/ui/button";
  import { Switch } from "$lib/components/ui/switch";
  import * as AlertDialog from "$lib/components/ui/alert-dialog";
  import Eye from "@lucide/svelte/icons/eye";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import { apps } from "$lib/stores/apps.svelte";
  import type { AppEntry } from "$lib/ipc";

  let { entry }: { entry: AppEntry } = $props();

  let confirmOpen = $state(false);
  let deleting = $state(false);

  const rt = $derived(apps.runtimeOf(entry.id));
  const isRunning = $derived(rt.status === "running");
  const crashed = $derived(
    rt.status === "stopped" && rt.exitCode != null && rt.exitCode !== 0,
  );
  const statusLabel = $derived.by(() => {
    if (rt.status === "running") return `PID ${rt.pid}`;
    if (rt.status === "starting") return "…";
    if (crashed) return `exit ${rt.exitCode}`;
    return "stopped";
  });

  async function onToggle(on: boolean): Promise<void> {
    try {
      if (on) await apps.start(entry.id);
      else await apps.stop(entry.id);
    } catch (e) {
      console.error(on ? "start failed" : "stop failed", entry.id, e);
    }
  }

  async function confirmDelete() {
    deleting = true;
    try {
      await apps.remove(entry.id);
      confirmOpen = false;
    } finally {
      deleting = false;
    }
  }
</script>

<div class="flex items-center gap-3 rounded-md border bg-card px-3 py-2">
  <span
    class="size-2.5 shrink-0 rounded-full {crashed ? 'bg-destructive' : ''}"
    style={crashed ? "" : `background-color: ${entry.tag}`}
    aria-hidden="true"
  ></span>
  <div class="min-w-0 flex-1">
    <div class="truncate text-sm font-medium">{entry.name}</div>
    <div
      class="truncate text-xs {crashed ? 'text-destructive' : 'text-muted-foreground'}"
    >
      {statusLabel}
    </div>
  </div>
  <Switch
    checked={isRunning}
    onCheckedChange={onToggle}
    disabled={rt.status === "starting"}
    aria-label="Toggle {entry.name}"
  />
  <Button variant="ghost" size="icon" disabled aria-label="View output">
    <Eye class="size-4" />
  </Button>
  <AlertDialog.Root bind:open={confirmOpen}>
    <AlertDialog.Trigger>
      {#snippet child({ props })}
        <Button
          {...props}
          variant="ghost"
          size="icon"
          aria-label="Delete {entry.name}"
        >
          <Trash2 class="size-4" />
        </Button>
      {/snippet}
    </AlertDialog.Trigger>
    <AlertDialog.Content>
      <AlertDialog.Header>
        <AlertDialog.Title>Delete "{entry.name}"?</AlertDialog.Title>
        <AlertDialog.Description>
          This removes the entry from your apps list. It does not delete any
          files on disk.
        </AlertDialog.Description>
      </AlertDialog.Header>
      <AlertDialog.Footer>
        <AlertDialog.Cancel disabled={deleting}>Cancel</AlertDialog.Cancel>
        <AlertDialog.Action disabled={deleting} onclick={confirmDelete}>
          Delete
        </AlertDialog.Action>
      </AlertDialog.Footer>
    </AlertDialog.Content>
  </AlertDialog.Root>
</div>
