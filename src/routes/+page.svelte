<script lang="ts">
  import { onMount } from "svelte";
  import { ScrollArea } from "$lib/components/ui/scroll-area";
  import AddAppDialog from "$lib/components/AddAppDialog.svelte";
  import AppRow from "$lib/components/AppRow.svelte";
  import TerminalPanel from "$lib/components/TerminalPanel.svelte";
  import { apps } from "$lib/stores/apps.svelte";
  import { dndzone, type DndEvent } from "svelte-dnd-action";
  import type { AppEntry } from "$lib/ipc";

  // svelte-dnd-action doesn't ship a `dragHandleSelector` option. To restrict
  // drag initiation to the handle, we keep dragging disabled by default and
  // flip it on when the user presses the handle. The dnd events then reset it.
  let dragDisabled = $state(true);

  function startDrag(e: Event) {
    e.preventDefault();
    dragDisabled = false;
  }

  function onConsider(e: CustomEvent<DndEvent<AppEntry>>) {
    apps.setOrder(e.detail.items);
  }
  function onFinalize(e: CustomEvent<DndEvent<AppEntry>>) {
    apps.setOrder(e.detail.items);
    apps
      .reorder(e.detail.items.map((a) => a.id))
      .catch((err) => console.error("reorder failed", err));
    dragDisabled = true;
  }

  onMount(() => {
    apps.init().catch((e) => console.error("apps.init failed", e));
  });

  const focusedName = $derived(
    apps.focusedId == null
      ? ""
      : (apps.apps.find((a) => a.id === apps.focusedId)?.name ?? ""),
  );
</script>

<div class="grid h-screen grid-rows-[auto_1fr]">
  <header
    class="flex items-center justify-between border-b px-4 py-3"
  >
    <h1 class="text-base font-semibold tracking-tight">Switchboard</h1>
    <AddAppDialog />
  </header>

  <div class="grid grid-cols-[minmax(320px,1fr)_2fr] overflow-hidden">
    <aside class="border-r">
      <ScrollArea class="h-full">
        <div class="flex flex-col gap-2 p-3">
          {#if !apps.loaded}
            <p class="px-2 py-8 text-center text-sm text-muted-foreground">
              Loading...
            </p>
          {:else if apps.apps.length === 0}
            <p class="px-2 py-8 text-center text-sm text-muted-foreground">
              No apps yet — click + Add
            </p>
          {:else}
            <div
              class="flex flex-col gap-2"
              use:dndzone={{
                items: apps.apps,
                dragDisabled,
                dropTargetStyle: {},
                flipDurationMs: 200,
              }}
              onconsider={onConsider}
              onfinalize={onFinalize}
            >
              {#each apps.apps as entry (entry.id)}
                <div id={entry.id}>
                  <AppRow {entry} {startDrag} />
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </ScrollArea>
    </aside>

    <main class="min-h-0 overflow-hidden">
      {#if apps.focusedId == null}
        <div class="flex h-full items-center justify-center p-6">
          <p class="text-sm text-muted-foreground">
            Select an app to view output
          </p>
        </div>
      {:else}
        {#key apps.focusedId}
          <TerminalPanel id={apps.focusedId} name={focusedName} />
        {/key}
      {/if}
    </main>
  </div>
</div>
