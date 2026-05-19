<script lang="ts">
  import { onMount } from "svelte";
  import { ScrollArea } from "$lib/components/ui/scroll-area";
  import AddAppDialog from "$lib/components/AddAppDialog.svelte";
  import AppRow from "$lib/components/AppRow.svelte";
  import { apps } from "$lib/stores/apps.svelte";

  onMount(() => {
    apps.init().catch((e) => console.error("apps.init failed", e));
  });
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
            {#each apps.apps as entry (entry.id)}
              <AppRow {entry} />
            {/each}
          {/if}
        </div>
      </ScrollArea>
    </aside>

    <main class="flex items-center justify-center p-6">
      <p class="text-sm text-muted-foreground">
        Select an app to view output
      </p>
    </main>
  </div>
</div>
