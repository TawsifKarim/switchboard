<script lang="ts">
  import { onDestroy } from "svelte";
  import XtermView from "$lib/components/XtermView.svelte";
  import { Button } from "$lib/components/ui/button";
  import ChevronUp from "@lucide/svelte/icons/chevron-up";
  import X from "@lucide/svelte/icons/x";
  import { apps } from "$lib/stores/apps.svelte";
  import { closeShell, openShell } from "$lib/ipc";

  let { id, name }: { id: string; name: string } = $props();

  // One-off shell drawer state. shellId is the backend's `oneoff:<ULID>` id
  // returned by open_shell; we hand it straight to XtermView to mount a
  // second terminal bound to that shell.
  let shellId = $state<string | null>(null);
  let openingShell = $state(false);

  const directory = $derived(
    apps.apps.find((a) => a.id === id)?.directory ?? "",
  );
  const truncatedDir = $derived(
    directory.length > 50 ? "…" + directory.slice(-49) : directory,
  );

  async function openDrawer(): Promise<void> {
    if (shellId || openingShell || !directory) return;
    openingShell = true;
    try {
      shellId = await openShell(directory);
    } catch (e) {
      console.error("openShell failed", e);
    } finally {
      openingShell = false;
    }
  }

  async function closeDrawer(): Promise<void> {
    const sid = shellId;
    if (!sid) return;
    // Tear down the UI immediately so XtermView's onDestroy fires (which
    // calls detach_pty); then ask the backend to kill the shell.
    shellId = null;
    try {
      await closeShell(sid);
    } catch (e) {
      console.error("closeShell failed", e);
    }
  }

  // Parent uses {#key apps.focusedId} so switching focused app destroys this
  // component, which fires this onDestroy and tears down any open shell.
  onDestroy(() => {
    if (shellId) {
      // Fire-and-forget; the user is already navigating away.
      closeShell(shellId).catch(() => {});
      shellId = null;
    }
  });
</script>

<div class="flex h-full flex-col">
  <div class="border-b px-3 py-2 text-sm font-medium">Terminal: {name}</div>
  <div class="relative flex min-h-0 flex-1 flex-col">
    <div
      class="min-h-0 overflow-hidden transition-[height] duration-200 ease-out"
      style={shellId ? "height: 55%" : "height: 100%"}
    >
      <XtermView {id} class="h-full w-full" />
    </div>

    {#if shellId}
      <div
        class="flex flex-col overflow-hidden border-t transition-[height] duration-200 ease-out"
        style="height: 45%"
      >
        <div
          class="flex items-center justify-between border-b bg-card px-3 py-1.5 text-xs"
        >
          <span class="truncate text-muted-foreground">
            Shell: {truncatedDir}
          </span>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Close shell"
            onclick={closeDrawer}
          >
            <X class="size-4" />
          </Button>
        </div>
        <div class="min-h-0 flex-1">
          <XtermView id={shellId} class="h-full w-full" />
        </div>
      </div>
    {:else}
      <Button
        variant="outline"
        size="icon-sm"
        aria-label="Open one-off shell"
        title="Open shell in {directory || 'directory'}"
        class="absolute right-3 bottom-3 z-10 shadow-sm"
        disabled={openingShell || !directory}
        onclick={openDrawer}
      >
        <ChevronUp class="size-4" />
      </Button>
    {/if}
  </div>
</div>
