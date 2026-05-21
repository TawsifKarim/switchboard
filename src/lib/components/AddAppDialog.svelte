<script lang="ts">
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import * as Dialog from "$lib/components/ui/dialog";
  import Plus from "@lucide/svelte/icons/plus";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { apps } from "$lib/stores/apps.svelte";
  import type { ReadyProbe } from "$lib/ipc";

  let open = $state(false);
  let name = $state("");
  let directory = $state("");
  let command = $state("");
  let tag = $state("#64748b");
  let portStr = $state("");
  let error = $state("");
  let submitting = $state(false);

  // Readiness probe form state. Stored as strings so the UI can validate
  // them the same way the port input does — the backend coerces.
  let readyKind = $state<"none" | "tcp" | "http" | "log_regex">("none");
  let readyTcpPort = $state("");
  let readyHttpUrl = $state("");
  let readyHttpStatus = $state("");
  let readyLogPattern = $state("");

  // Dependency selection: ids of existing apps this one depends on. The
  // current app doesn't exist yet at add time so there's no self-dep risk.
  let dependsOn = $state<string[]>([]);
  function toggleDep(id: string, on: boolean) {
    if (on) {
      if (!dependsOn.includes(id)) dependsOn = [...dependsOn, id];
    } else {
      dependsOn = dependsOn.filter((x) => x !== id);
    }
  }

  const portIsValid = $derived.by(() => {
    if (portStr.trim() === "") return true;
    const n = Number(portStr);
    return Number.isInteger(n) && n >= 1 && n <= 65535;
  });

  const readyIsValid = $derived.by(() => {
    if (readyKind === "none") return true;
    if (readyKind === "tcp") {
      const n = Number(readyTcpPort);
      return Number.isInteger(n) && n >= 1 && n <= 65535;
    }
    if (readyKind === "http") {
      if (readyHttpUrl.trim() === "") return false;
      if (readyHttpStatus.trim() !== "") {
        const n = Number(readyHttpStatus);
        if (!Number.isInteger(n) || n < 100 || n > 599) return false;
      }
      return true;
    }
    if (readyKind === "log_regex") return readyLogPattern.trim() !== "";
    return true;
  });

  let canSubmit = $derived(
    name.trim().length > 0 &&
      directory.trim().length > 0 &&
      command.trim().length > 0 &&
      portIsValid &&
      readyIsValid &&
      !submitting,
  );

  function buildProbe(): ReadyProbe | null {
    switch (readyKind) {
      case "tcp":
        return { kind: "tcp", port: Number(readyTcpPort) };
      case "http":
        return {
          kind: "http",
          url: readyHttpUrl.trim(),
          expect_status:
            readyHttpStatus.trim() === "" ? null : Number(readyHttpStatus),
        };
      case "log_regex":
        return { kind: "log_regex", pattern: readyLogPattern };
      default:
        return null;
    }
  }

  function reset() {
    name = "";
    directory = "";
    command = "";
    tag = "#64748b";
    portStr = "";
    readyKind = "none";
    readyTcpPort = "";
    readyHttpUrl = "";
    readyHttpStatus = "";
    readyLogPattern = "";
    dependsOn = [];
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
      const probe = buildProbe();
      await apps.add(
        name.trim(),
        directory.trim(),
        command.trim(),
        tag,
        port,
        probe,
        dependsOn,
      );
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
          type="text"
          inputmode="numeric"
          pattern="[0-9]*"
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
        <Label for="app-ready-kind">
          Readiness check <span class="text-muted-foreground">(optional)</span>
        </Label>
        <select
          id="app-ready-kind"
          bind:value={readyKind}
          class="border-input bg-background h-9 rounded-md border px-3 text-sm shadow-sm focus-visible:ring-1 focus-visible:outline-none"
        >
          <option value="none">None</option>
          <option value="tcp">TCP port</option>
          <option value="http">HTTP endpoint</option>
          <option value="log_regex">Log line match</option>
        </select>
        {#if readyKind === "tcp"}
          <Input
            type="text"
            inputmode="numeric"
            pattern="[0-9]*"
            bind:value={readyTcpPort}
            placeholder="8080"
            aria-label="TCP port"
          />
        {:else if readyKind === "http"}
          <Input
            type="text"
            bind:value={readyHttpUrl}
            placeholder="http://localhost:8080/healthz"
            aria-label="HTTP URL"
          />
          <Input
            type="text"
            inputmode="numeric"
            pattern="[0-9]*"
            bind:value={readyHttpStatus}
            placeholder="Expected status (blank = any 2xx/3xx)"
            aria-label="Expected status"
          />
        {:else if readyKind === "log_regex"}
          <Input
            type="text"
            bind:value={readyLogPattern}
            placeholder="listening on"
            aria-label="Log regex"
          />
        {/if}
        {#if readyKind !== "none" && !readyIsValid}
          <p class="text-xs text-destructive">Fill out the probe fields.</p>
        {:else if readyKind !== "none"}
          <p class="text-xs text-muted-foreground">
            Service is "ready" once the probe succeeds (60s timeout).
          </p>
        {/if}
      </div>
      {#if apps.apps.length > 0}
        <div class="grid gap-1.5">
          <Label>
            Depends on <span class="text-muted-foreground">(optional)</span>
          </Label>
          <div
            class="grid max-h-32 gap-1 overflow-y-auto rounded-md border bg-background p-2"
          >
            {#each apps.apps as parent (parent.id)}
              <label class="flex cursor-pointer items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={dependsOn.includes(parent.id)}
                  onchange={(e) =>
                    toggleDep(parent.id, (e.currentTarget as HTMLInputElement).checked)}
                />
                <span
                  class="inline-block size-2 rounded-full"
                  style="background-color: {parent.tag}"
                  aria-hidden="true"
                ></span>
                <span class="truncate">{parent.name}</span>
              </label>
            {/each}
          </div>
          <p class="text-xs text-muted-foreground">
            Start All waits for each parent to report ready before starting this app.
          </p>
        </div>
      {/if}
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
