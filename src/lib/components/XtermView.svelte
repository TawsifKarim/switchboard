<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";

  // Reusable xterm mount bound to a process id. Owns the attach/detach
  // lifecycle, the ResizeObserver, and the `app-started` re-attach so the
  // panel can be reused for the main app terminal AND the one-off shell
  // drawer with no per-call wiring.
  let { id, class: className = "" }: { id: string; class?: string } = $props();

  let containerEl: HTMLDivElement | undefined;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let unlistenData: UnlistenFn | null = null;
  let unlistenStarted: UnlistenFn | null = null;
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
          () => {},
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
      invoke("write_pty", { id, data }).catch(() => {});
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

    // Re-attach when the same app is restarted while this panel is focused.
    // Harmless for one-off shells: their id is a fresh ULID that never matches
    // an `app-started` event (shells don't emit `app-started`).
    unlistenStarted = await listen<{ id: string; pid: number }>(
      "app-started",
      async (e) => {
        if (disposed || !term || e.payload.id !== id) return;
        term.clear();
        try {
          await invoke("attach_pty", { id });
          await invoke("resize_pty", { id, rows: term.rows, cols: term.cols });
        } catch {
          // ignore; the user may have toggled off again
        }
      },
    );

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
    unlistenStarted?.();
    unlistenStarted = null;
    invoke("detach_pty", { id }).catch(() => {});
    term?.dispose();
    term = undefined;
    fit = undefined;
  });
</script>

<div
  bind:this={containerEl}
  class="overflow-hidden bg-[#0a0a0a] {className}"
></div>
