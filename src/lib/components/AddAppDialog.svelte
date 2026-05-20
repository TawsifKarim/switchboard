<script lang="ts">
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import * as Dialog from "$lib/components/ui/dialog";
  import Plus from "@lucide/svelte/icons/plus";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { apps } from "$lib/stores/apps.svelte";

  let open = $state(false);
  let name = $state("");
  let directory = $state("");
  let command = $state("");
  let tag = $state("#64748b");
  let portStr = $state("");
  let error = $state("");
  let submitting = $state(false);

  const portIsValid = $derived.by(() => {
    if (portStr.trim() === "") return true;
    const n = Number(portStr);
    return Number.isInteger(n) && n >= 1 && n <= 65535;
  });

  let canSubmit = $derived(
    name.trim().length > 0 &&
      directory.trim().length > 0 &&
      command.trim().length > 0 &&
      portIsValid &&
      !submitting,
  );

  function reset() {
    name = "";
    directory = "";
    command = "";
    tag = "#64748b";
    portStr = "";
    error = "";
    submitting = false;
  }

  async function browse() {
    try {
      const selected = await openDialog({ directory: true, multiple: false });
      if (typeof selected === "string") directory = selected;
    } catch (e) {
      error = String(e);
    }
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    error = "";
    submitting = true;
    try {
      const port = portStr.trim() === "" ? null : Number(portStr);
      await apps.add(name.trim(), directory.trim(), command.trim(), tag, port);
      open = false;
      reset();
    } catch (e) {
      error = String(e);
    } finally {
      submitting = false;
    }
  }

  $effect(() => {
    if (!open) reset();
  });
</script>

<Dialog.Root bind:open>
  <Dialog.Trigger>
    {#snippet child({ props })}
      <Button {...props} size="sm">
        <Plus class="size-4" />
        Add
      </Button>
    {/snippet}
  </Dialog.Trigger>
  <Dialog.Content class="sm:max-w-md">
    <Dialog.Header>
      <Dialog.Title>Add an app</Dialog.Title>
      <Dialog.Description>
        Pick a directory and a command to run there.
      </Dialog.Description>
    </Dialog.Header>
    <form class="grid gap-4" onsubmit={submit}>
      <div class="grid gap-1.5">
        <Label for="app-name">Name</Label>
        <Input id="app-name" bind:value={name} placeholder="auth-service" />
      </div>
      <div class="grid gap-1.5">
        <Label for="app-directory">Directory</Label>
        <div class="flex gap-2">
          <Input
            id="app-directory"
            bind:value={directory}
            placeholder="/Users/you/Projects/auth"
            class="flex-1"
          />
          <Button type="button" variant="outline" onclick={browse}>
            <FolderOpen class="size-4" />
            Browse
          </Button>
        </div>
      </div>
      <div class="grid gap-1.5">
        <Label for="app-command">Command</Label>
        <Input id="app-command" bind:value={command} placeholder="air" />
      </div>
      <div class="grid gap-1.5">
        <Label for="app-port">Port <span class="text-muted-foreground">(optional)</span></Label>
        <Input
          id="app-port"
          type="number"
          min="1"
          max="65535"
          bind:value={portStr}
          placeholder="8080"
        />
        {#if portStr.trim() !== "" && !portIsValid}
          <p class="text-xs text-destructive">Port must be 1–65535.</p>
        {:else}
          <p class="text-xs text-muted-foreground">
            When set, anything bound to this port is killed before start and after stop.
          </p>
        {/if}
      </div>
      <div class="grid gap-1.5">
        <Label for="app-tag">Tag color</Label>
        <input
          id="app-tag"
          type="color"
          bind:value={tag}
          class="h-9 w-16 cursor-pointer rounded-md border bg-background p-1"
        />
      </div>
      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
      <Dialog.Footer>
        <Button type="button" variant="ghost" onclick={() => (open = false)}>
          Cancel
        </Button>
        <Button type="submit" disabled={!canSubmit}>
          {submitting ? "Adding..." : "Add"}
        </Button>
      </Dialog.Footer>
    </form>
  </Dialog.Content>
</Dialog.Root>
