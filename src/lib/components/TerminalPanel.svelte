<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";

  let { id, name }: { id: string; name: string } = $props();

  let containerEl: HTMLDivElement | undefined;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let unlistenData: UnlistenFn | null = null;
  let onDataDisposer: { dispose: () => void } | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let resizeTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  function b64ToBytes(s: string): Uint8Array {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  function scheduleResize(): void {
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      if (disposed || !term || !fit) return;
      try {
        fit.fit();
        invoke("resize_pty", { id, rows: term.rows, cols: term.cols }).catch(
          () => {
            // App may have exited between attach and resize; ignore.
          },
        );
      } catch {
        // fit can throw if the container has zero dimensions during a transition.
      }
    }, 80);
  }

  onMount(async () => {
    if (!containerEl) return;
    term = new Terminal({
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
      fontSize: 13,
      cursorBlink: true,
      convertEol: false,
      theme: { background: "#0a0a0a" },
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerEl);
    fit.fit();

    onDataDisposer = term.onData((data) => {
      invoke("write_pty", { id, data }).catch(() => {
        // Process may not be running; ignore writes.
      });
    });

    // Register the data listener BEFORE attach. The Rust attach path emits
    // the scrollback snapshot synchronously, so a listener registered after
    // attach() returns would miss the replay event entirely.
    unlistenData = await listen<string>(`pty:${id}:data`, (e) => {
      if (!term) return;
      term.write(b64ToBytes(e.payload));
    });

    try {
      await invoke("attach_pty", { id });
    } catch (e) {
      term.write(`\r\n\x1b[31mattach failed: ${e}\x1b[0m\r\n`);
      return;
    }

    try {
      await invoke("resize_pty", { id, rows: term.rows, cols: term.cols });
    } catch {
      // ignore
    }

    resizeObserver = new ResizeObserver(scheduleResize);
    resizeObserver.observe(containerEl);
    window.addEventListener("resize", scheduleResize);
  });

  onDestroy(() => {
    disposed = true;
    if (resizeTimer) clearTimeout(resizeTimer);
    window.removeEventListener("resize", scheduleResize);
    resizeObserver?.disconnect();
    resizeObserver = null;
    onDataDisposer?.dispose();
    onDataDisposer = null;
    unlistenData?.();
    unlistenData = null;
    invoke("detach_pty", { id }).catch(() => {
      // App may have exited already; harmless.
    });
    term?.dispose();
    term = undefined;
    fit = undefined;
  });
</script>

<div class="flex h-full flex-col">
  <div class="border-b px-3 py-2 text-sm font-medium">Terminal: {name}</div>
  <div
    bind:this={containerEl}
    class="min-h-0 flex-1 overflow-hidden bg-[#0a0a0a]"
  ></div>
</div>
