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
    class="size-2.5 shrink-0 rounded-full"
    style="background-color: {entry.tag}"
    aria-hidden="true"
  ></span>
  <div class="min-w-0 flex-1">
    <div class="truncate text-sm font-medium">{entry.name}</div>
    <div class="truncate text-xs text-muted-foreground">stopped</div>
  </div>
  <Switch disabled aria-label="Toggle {entry.name}" />
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
