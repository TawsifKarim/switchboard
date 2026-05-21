<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { Button } from "$lib/components/ui/button";
  import { Switch } from "$lib/components/ui/switch";
  import * as AlertDialog from "$lib/components/ui/alert-dialog";
  import Eye from "@lucide/svelte/icons/eye";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import Loader2 from "@lucide/svelte/icons/loader-2";
  import GripVertical from "@lucide/svelte/icons/grip-vertical";
  import GitBranch from "@lucide/svelte/icons/git-branch";
  import CornerDownRight from "@lucide/svelte/icons/corner-down-right";
  import { apps } from "$lib/stores/apps.svelte";
  import { getBranch, type AppEntry } from "$lib/ipc";

  let {
    entry,
    startDrag,
  }: { entry: AppEntry; startDrag?: (e: Event) => void } = $props();

  let confirmOpen = $state(false);
  let deleting = $state(false);
  let branch = $state<string | null>(null);

  async function refreshBranch() {
    try {
      branch = await getBranch(entry.directory);
    } catch {
      branch = null;
    }
  }

  let unlistenStarted: UnlistenFn | null = null;
  onMount(() => {
    refreshBranch();
    listen<{ id: string; pid: number }>("app-started", (e) => {
      if (e.payload.id === entry.id) refreshBranch();
    })
      .then((u) => {
        unlistenStarted = u;
      })
      .catch(() => {});
  });
  onDestroy(() => {
    if (unlistenStarted) unlistenStarted();
  });

  const rt = $derived(apps.runtimeOf(entry.id));
  const isRunning = $derived(rt.status === "running");
  const isStopping = $derived(rt.status === "stopping");
  const isFocused = $derived(apps.focusedId === entry.id);
  const crashed = $derived(
    rt.status === "stopped" && rt.exitCode != null && rt.exitCode !== 0,
  );
  // Dot opacity: full when ready/idle, dim while waiting for the probe (or
  // while starting). Crashed paths bypass this — destructive red is its own
  // signal. Stopped (clean) stays full so the user sees the tag color.
  const dotDim = $derived(
    rt.status === "starting" || (isRunning && !rt.ready),
  );
  const stats = $derived(apps.stats[entry.id]);
  function formatRss(bytes: number): string {
    if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} B`;
  }
  // Display the dep parents by name (not id). Truncate the combined string so
  // the row layout stays predictable on a narrow split.
  const depsLabel = $derived.by(() => {
    const ids = entry.depends_on ?? [];
    if (ids.length === 0) return "";
    const byId = new Map(apps.apps.map((a) => [a.id, a.name]));
    const names = ids.map((id) => byId.get(id) ?? id.slice(0, 6));
    const joined = names.join(", ");
    return joined.length > 32 ? joined.slice(0, 31) + "…" : joined;
  });

  const fallbackLabel = $derived.by(() => {
    if (rt.status === "starting") return "…";
    if (rt.status === "stopping") return "terminating…";
    if (crashed) return `exit ${rt.exitCode}`;
    return "stopped";
  });

  async function onToggle(on: boolean): Promise<void> {
    apps.focus(entry.id);
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

<div
  class="flex items-center gap-3 rounded-md border border-l-[3px] px-3 py-2 {isFocused
    ? 'bg-accent'
    : 'bg-card'}"
  style={crashed
    ? "border-left-color: var(--destructive)"
    : `border-left-color: ${entry.tag}; opacity: ${dotDim ? 0.85 : 1}`}
  aria-label={isRunning ? (rt.ready ? "ready" : "starting") : "stopped"}
>
  <span
    role="button"
    tabindex="-1"
    aria-label="Drag to reorder"
    class="flex shrink-0 cursor-grab text-muted-foreground/60 select-none hover:text-muted-foreground active:cursor-grabbing"
    onpointerdown={startDrag}
    ontouchstart={startDrag}
  >
    <GripVertical class="size-4" />
  </span>
  <div class="min-w-0 flex-1">
    <div class="truncate text-sm font-medium">{entry.name}</div>
    {#if branch}
      <div
        class="flex items-center gap-1 truncate text-[11px] text-muted-foreground tabular-nums"
      >
        <GitBranch class="size-3 shrink-0" />
        <span class="truncate">{branch.length > 24 ? branch.slice(0, 23) + "…" : branch}</span>
      </div>
    {/if}
    {#if depsLabel}
      <div
        class="flex items-center gap-1 truncate text-[11px] text-muted-foreground"
      >
        <CornerDownRight class="size-3 shrink-0" />
        <span class="truncate">depends on: {depsLabel}</span>
      </div>
    {/if}
    <div
      class="flex items-center gap-1.5 truncate text-xs {crashed ? 'text-destructive' : 'text-muted-foreground'}"
    >
      {#if isStopping}
        <Loader2 class="size-3 animate-spin" />
      {/if}
      {#if isRunning}
        <span class="flex items-center gap-3 tabular-nums">
          <span class="inline-block min-w-[10ch]">
            <span class="opacity-60">PID:</span> {rt.pid}
          </span>
          {#if entry.port != null}
            <span class="inline-block min-w-[10ch]">
              <span class="opacity-60">PORT:</span> {entry.port}
            </span>
          {/if}
          {#if stats}
            <span class="inline-block min-w-[8ch]">
              <span class="opacity-60">CPU:</span> {Math.round(stats.cpu_pct)}%
            </span>
            <span class="inline-block min-w-[11ch]">
              <span class="opacity-60">RAM:</span> {formatRss(stats.rss_bytes)}
            </span>
          {/if}
        </span>
      {:else}
        <span class="truncate">{fallbackLabel}</span>
        {#if entry.port != null}
          <span class="text-muted-foreground/80">:{entry.port}</span>
        {/if}
      {/if}
    </div>
  </div>
  <Switch
    checked={isRunning}
    onCheckedChange={onToggle}
    disabled={rt.status === "starting" || isStopping}
    aria-label="Toggle {entry.name}"
  />
  <Button
    variant="ghost"
    size="icon"
    aria-label="View output"
    aria-pressed={isFocused}
    onclick={() => apps.focus(entry.id)}
  >
    <Eye class="size-4" />
  </Button>
  <Button
    variant="ghost"
    size="icon"
    aria-label="Delete {entry.name}"
    onclick={() => (confirmOpen = true)}
  >
    <Trash2 class="size-4" />
  </Button>
  <AlertDialog.Root bind:open={confirmOpen}>
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
