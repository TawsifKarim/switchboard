use crate::config::{AppEntry, ReadyProbe};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use sysinfo::{Pid as SysPid, ProcessesToUpdate, System};
use tauri::Emitter;
use tokio::sync::{broadcast, Notify};
use tokio::task::AbortHandle;
use ulid::Ulid;

/// Prefix used for one-off shell sessions opened from the terminal drawer.
/// Lets the tray running count and the stats sampler skip them so transient
/// shells aren't reported as user apps.
const SHELL_ID_PREFIX: &str = "oneoff:";

#[cfg(unix)]
use nix::sys::signal::{kill, killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

/// Called when a child exits. The id is the app id; the i32 is the exit code.
pub type ExitCallback = Box<dyn Fn(&str, i32) + Send + Sync + 'static>;

// Shorter in tests so the timeout path can be exercised without 60s waits.
#[cfg(not(test))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(test)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

const PROBE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);

/// Caps for the per-app scrollback ring buffer.
/// `CAP_LINES` is the user-facing scrollback budget; `CAP_BYTES` is a hard
/// safety cap so a single absurd line doesn't pin a megabyte per app.
const RING_CAP_LINES: usize = 300;
const RING_CAP_BYTES: usize = 512 * 1024;

/// Hard size cap for a per-app on-disk PTY log. When the live `<id>.log`
/// passes this, it rotates to `<id>.log.1` (one generation kept) so a chatty
/// or long-running service can't grow its log without bound. Worst-case
/// on-disk per app is ~2× this.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Per-app rolling buffer of recent PTY bytes. Trims from the front when
/// either the line or byte budget is exceeded.
struct RingBuffer {
    chunks: VecDeque<Vec<u8>>,
    line_count: usize,
    cap_lines: usize,
    cap_bytes: usize,
    total_bytes: usize,
}

impl RingBuffer {
    fn new(cap_lines: usize, cap_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            line_count: 0,
            cap_lines,
            cap_bytes,
            total_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let added_lines = bytecount_newlines(bytes);
        self.chunks.push_back(bytes.to_vec());
        self.line_count += added_lines;
        self.total_bytes += bytes.len();
        while self.line_count > self.cap_lines || self.total_bytes > self.cap_bytes {
            let Some(front) = self.chunks.pop_front() else {
                break;
            };
            self.line_count = self
                .line_count
                .saturating_sub(bytecount_newlines(&front));
            self.total_bytes = self.total_bytes.saturating_sub(front.len());
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        out
    }
}

fn bytecount_newlines(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

/// Append-only PTY log with a hard size cap. The reader loop writes every byte
/// a child emits here; without a cap a chatty service (e.g. one retrying a
/// failed connection once a second forever) grows it without bound. When the
/// live file would pass `MAX_LOG_BYTES` it rotates to `<id>.log.1`, keeping one
/// generation so recent scrollback survives the boundary.
struct RotatingLog {
    path: PathBuf,
    file: File,
    written: u64,
}

impl RotatingLog {
    /// Open (creating if needed) in append mode. Seeds `written` from the
    /// current on-disk size so an already-oversized file rotates on first write
    /// instead of growing further.
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            written,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if self.written + bytes.len() as u64 > MAX_LOG_BYTES {
            self.rotate()?;
        }
        self.file.write_all(bytes)?;
        self.file.flush()?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// Rename the live file to `<id>.log.1` (replacing any prior generation) and
    /// reopen a fresh empty live file.
    fn rotate(&mut self) -> std::io::Result<()> {
        if let Some(name) = self.path.file_name().and_then(|n| n.to_str()) {
            let rotated = self.path.with_file_name(format!("{name}.1"));
            std::fs::rename(&self.path, &rotated)?;
        }
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusSnapshot {
    pub running: bool,
    pub pid: Option<u32>,
    pub last_exit: Option<i32>,
    pub ready: bool,
}

/// Payload for the global `app-ready` event. `ready` is true when the probe
/// succeeded; false means it timed out (the process is still alive, but the
/// service never reported healthy).
#[derive(Clone, Debug, Serialize)]
pub struct ReadyPayload {
    pub id: String,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Payload for the global `app-exit` event. Carries the id so a single
/// listener on the frontend can route by app id.
#[derive(Clone, Debug, Serialize)]
pub struct ExitPayload {
    pub id: String,
    pub code: i32,
}

/// Payload for the global `app-started` event.
#[derive(Clone, Debug, Serialize)]
pub struct StartedPayload {
    pub id: String,
    pub pid: u32,
}

/// Payload for the global `app-stats` event. Emitted every ~2s per running app.
/// `cpu_pct` is summed across the root pid and all descendants; on a multi-core
/// machine a single CPU-pegged process can briefly read above 100%.
#[derive(Clone, Debug, Serialize)]
pub struct StatsPayload {
    pub id: String,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
}

/// Sampling cadence for CPU + RAM. 2s is a comfortable balance — `sysinfo`
/// reports CPU as a delta since the previous refresh, so sub-second sampling
/// makes numbers noisy.
const STATS_INTERVAL: Duration = Duration::from_secs(2);

pub struct RunningApp {
    pub pid: u32,
    pub broadcast_tx: broadcast::Sender<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    notify_exit: Arc<Notify>,
    /// Cached at start time so stop() can run the post-stop port sweep
    /// without re-reading the config.
    port: Option<u16>,
}

struct Inner {
    running: HashMap<String, RunningApp>,
    last_exit: HashMap<String, i32>,
    /// Ids whose start() is mid-flight (spawn between dup-check and insert).
    /// Prevents a concurrent second start from passing the dup-check and
    /// leaking a child by overwriting the first RunningApp.
    starting: std::collections::HashSet<String>,
    /// Background tasks forwarding PTY bytes to Tauri events. Keyed by app id.
    /// Lets attach/detach be idempotent: detach aborts the existing task.
    attachments: HashMap<String, AbortHandle>,
    /// Recent PTY output per app id. Survives process exit so the user can
    /// still scroll back the tail of a crashed app. Reset on each new start.
    /// Cleared explicitly when the app entry is deleted.
    recent_output: HashMap<String, RingBuffer>,
    /// Per-app readiness flag — true once the configured probe has resolved
    /// positively for the current run. Reset to false on every start; removed
    /// on stop / clear_ring. Apps with no probe go ready=true as soon as the
    /// PTY spawns (see spawn_pty).
    ready: HashMap<String, bool>,
    /// Set to true once `start_stats_sampler` has spawned its background task.
    /// Prevents double-spawn if setup somehow runs more than once.
    sampler_started: bool,
}

pub struct ProcessManager {
    inner: Arc<Mutex<Inner>>,
    log_dir: PathBuf,
    /// Broadcasts every probe resolution (positive or negative) for orderly
    /// `start_all` to await without polling. Independent from the per-app PTY
    /// broadcast — capacity is small because slow consumers can lag a few
    /// events without breaking correctness (start_all reconciles via status).
    ready_broadcast: broadcast::Sender<ReadyPayload>,
}

impl ProcessManager {
    pub fn new(log_dir: PathBuf) -> Self {
        let (ready_broadcast, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                running: HashMap::new(),
                last_exit: HashMap::new(),
                starting: std::collections::HashSet::new(),
                attachments: HashMap::new(),
                recent_output: HashMap::new(),
                ready: HashMap::new(),
                sampler_started: false,
            })),
            log_dir,
            ready_broadcast,
        }
    }

    /// Subscribe to per-app readiness resolutions. Subscribe BEFORE calling
    /// `start` if you need to deterministically observe the event (probe-less
    /// apps fire synchronously inside spawn_pty).
    pub fn subscribe_ready(&self) -> broadcast::Receiver<ReadyPayload> {
        self.ready_broadcast.subscribe()
    }

    /// Spawn the background task that samples CPU + RAM for every running app
    /// every 2s and emits `app-stats` events. Idempotent — safe to call more
    /// than once.
    pub fn start_stats_sampler(&self, app: tauri::AppHandle) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.sampler_started {
                return;
            }
            inner.sampler_started = true;
        }
        let inner = self.inner.clone();
        // Use Tauri's async runtime — at setup time there is no enclosing
        // Tokio runtime context, so `tokio::spawn` panics with "no reactor
        // running". Tauri's runtime is alive by the time setup fires.
        tauri::async_runtime::spawn(async move {
            // Owned System so refreshes between ticks measure CPU as a delta.
            let mut sys = System::new();
            // Prime once so the first emitted CPU value is meaningful.
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let mut tick = tokio::time::interval(STATS_INTERVAL);
            // Avoid burst-firing if the runtime falls behind.
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let snapshot: Vec<(String, u32)> = {
                    let g = inner.lock().unwrap();
                    g.running
                        .iter()
                        .filter(|(id, _)| !id.starts_with(SHELL_ID_PREFIX))
                        .map(|(id, app)| (id.clone(), app.pid))
                        .collect()
                };
                if snapshot.is_empty() {
                    continue;
                }
                sys.refresh_processes(ProcessesToUpdate::All, true);
                // Build parent → children index once per tick. O(N) total.
                let mut by_parent: HashMap<SysPid, Vec<SysPid>> = HashMap::new();
                for (pid, proc_) in sys.processes() {
                    if let Some(parent) = proc_.parent() {
                        by_parent.entry(parent).or_default().push(*pid);
                    }
                }
                for (id, root) in snapshot {
                    let tree = descendants_set(SysPid::from_u32(root), &by_parent);
                    let mut cpu: f32 = 0.0;
                    let mut rss: u64 = 0;
                    for p in &tree {
                        if let Some(proc_) = sys.process(*p) {
                            cpu += proc_.cpu_usage();
                            rss += proc_.memory();
                        }
                    }
                    let _ = app.emit(
                        "app-stats",
                        StatsPayload {
                            id,
                            cpu_pct: cpu,
                            rss_bytes: rss,
                        },
                    );
                }
            }
        });
    }

    /// Number of currently-running user apps. Used by the tray to update the
    /// "Running: N" label. Excludes transient one-off shell sessions.
    pub fn running_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .running
            .keys()
            .filter(|id| !id.starts_with(SHELL_ID_PREFIX))
            .count()
    }

    /// Production entry point: emit a Tauri event on exit and on ready.
    pub async fn start(&self, app: tauri::AppHandle, entry: AppEntry) -> Result<u32> {
        let app_for_exit = app.clone();
        let id_for_event = entry.id.clone();
        let on_exit: ExitCallback = Box::new(move |id, code| {
            let _ = app_for_exit.emit(
                "app-exit",
                ExitPayload {
                    id: id.to_string(),
                    code,
                },
            );
        });
        let pid = self
            .start_internal(entry, on_exit, Some(app.clone()))
            .await?;
        // Also emit a global app-started event so listeners (e.g. the tray
        // "Running: N" label) can refresh without polling.
        let _ = app.emit(
            "app-started",
            StartedPayload {
                id: id_for_event,
                pid,
            },
        );
        Ok(pid)
    }

    /// Test-only entry point: callback receives (id, exit_code) on child exit.
    /// Readiness resolutions go to `ready_broadcast` only — tests subscribe via
    /// `subscribe_ready()` BEFORE calling start to avoid races.
    #[cfg(test)]
    pub async fn start_with_callback(
        &self,
        entry: AppEntry,
        on_exit: ExitCallback,
    ) -> Result<u32> {
        self.start_internal(entry, on_exit, None).await
    }

    async fn start_internal(
        &self,
        entry: AppEntry,
        on_exit: ExitCallback,
        app_for_ready: Option<tauri::AppHandle>,
    ) -> Result<u32> {
        // Inherit env first so PATH/HOME/asdf shims/etc. resolve the same as
        // running the command by hand.
        let mut cmd = CommandBuilder::new("zsh");
        for (k, v) in std::env::vars_os() {
            cmd.env(k, v);
        }
        // Pass the user's command via an env var to avoid shell-quoting issues.
        cmd.env("SWITCHBOARD_USER_CMD", &entry.command);
        // The outer `zsh -ic` sources the user's .zshrc (so PATH is set up
        // like a real terminal), then `exec`s into a NON-interactive zsh that
        // evaluates the user's command. Going non-interactive is critical:
        // interactive zsh ignores SIGTERM at the C level, which would defeat
        // stop(). The exec means the outer interactive zsh disappears entirely
        // — the live pid runs non-interactive zsh (or the user command itself,
        // if zsh exec-optimizes a final simple command).
        cmd.arg("-ic");
        cmd.arg(r#"exec zsh -c "$SWITCHBOARD_USER_CMD""#);
        cmd.cwd(&entry.directory);

        self.spawn_pty(
            entry.id.clone(),
            cmd,
            entry.port,
            on_exit,
            entry.ready.clone(),
            app_for_ready,
        )
        .await
    }

    /// Spawn a one-off interactive login zsh in `directory` and register it
    /// in the running map under a `oneoff:<ULID>` id. Reuses the full PTY /
    /// broadcast / ring-buffer / log file plumbing so the existing `attach`,
    /// `write_pty`, `resize`, `stop`, and scrollback paths all work unchanged.
    pub async fn open_shell(
        &self,
        app: tauri::AppHandle,
        directory: String,
    ) -> Result<String> {
        let id = format!("{SHELL_ID_PREFIX}{}", Ulid::new());

        let mut cmd = CommandBuilder::new("zsh");
        for (k, v) in std::env::vars_os() {
            cmd.env(k, v);
        }
        // Interactive login shell — same affordances as opening a real
        // terminal: sources .zshrc / .zprofile, prompts, line editing.
        cmd.arg("-il");
        cmd.cwd(&directory);

        let app_for_cb = app.clone();
        let on_exit: ExitCallback = Box::new(move |id, code| {
            // Shells emit the same `app-exit` event shape as user apps so the
            // frontend store can clear any (unused) runtime entry it accreted.
            let _ = app_for_cb.emit(
                "app-exit",
                ExitPayload {
                    id: id.to_string(),
                    code,
                },
            );
        });

        // One-off shells never get a probe — they're "ready" the moment zsh
        // shows a prompt and we don't have a sensible probe semantics for them.
        self.spawn_pty(id.clone(), cmd, None, on_exit, None, None)
            .await?;
        Ok(id)
    }

    fn emit_ready(
        &self,
        id: &str,
        ready: bool,
        reason: Option<String>,
        app: &Option<tauri::AppHandle>,
    ) {
        // Two channels intentionally: WebKit listens via Tauri event;
        // Rust (start_all) via in-process broadcast.
        let payload = ReadyPayload {
            id: id.to_string(),
            ready,
            reason,
        };
        if let Some(app) = app {
            let _ = app.emit("app-ready", payload.clone());
        }
        let _ = self.ready_broadcast.send(payload);
    }

    /// Close a one-off shell: stop the underlying process and drop its
    /// scrollback. Idempotent — safe to call after the shell has exited.
    pub async fn close_shell(&self, id: &str) -> Result<()> {
        self.stop(id).await?;
        self.clear_ring(id);
        self.delete_logs(id);
        Ok(())
    }

    /// Shared spawn path used by both `start_with_callback` (user apps) and
    /// `open_shell` (one-off shells). Owns the dup-check race guard, port
    /// pre-flight, PTY/broadcast/ring/reader/waiter setup, and the final map
    /// insert. Returns the spawned PID.
    async fn spawn_pty(
        &self,
        id: String,
        cmd: CommandBuilder,
        port: Option<u16>,
        on_exit: ExitCallback,
        probe: Option<ReadyProbe>,
        app_for_ready: Option<tauri::AppHandle>,
    ) -> Result<u32> {
        // Atomic dup-check + claim. Holding `starting` for the duration of
        // spawn prevents a second concurrent start(id) from passing the
        // running-map check and leaking a child by overwriting the first
        // RunningApp on insert. Released on success (after insert) or via
        // the StartGuard's Drop on any error path. For shells the id is a
        // fresh ULID so the dup branch never fires — harmless.
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.running.contains_key(&id) || inner.starting.contains(&id) {
                return Err(anyhow!("app {id} already running"));
            }
            inner.starting.insert(id.clone());
        }
        struct StartGuard {
            inner: Arc<Mutex<Inner>>,
            id: String,
            armed: bool,
        }
        impl Drop for StartGuard {
            fn drop(&mut self) {
                if self.armed {
                    if let Ok(mut g) = self.inner.lock() {
                        g.starting.remove(&self.id);
                    }
                }
            }
        }
        let mut guard = StartGuard {
            inner: self.inner.clone(),
            id: id.clone(),
            armed: true,
        };

        // Pre-flight: free the configured port. Catches stale leftovers from
        // a previous run that didn't shut down cleanly. Shells pass None here.
        #[cfg(unix)]
        if let Some(p) = port {
            let _ = sweep_port(p).await;
        }

        std::fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("creating log dir {}", self.log_dir.display()))?;
        let log_path = self.log_dir.join(format!("{id}.log"));
        let log_file = RotatingLog::open(log_path.clone())
            .with_context(|| format!("opening log file {}", log_path.display()))?;

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty failed")?;

        let child = pair.slave.spawn_command(cmd).context("spawning command")?;
        let pid = child.process_id().context("child has no PID")?;

        // Drop the slave so the master sees EOF after the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .context("cloning pty reader")?;
        let writer = pair.master.take_writer().context("taking pty writer")?;

        let (broadcast_tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let notify_exit = Arc::new(Notify::new());

        let master = Arc::new(Mutex::new(pair.master));
        let pty_writer = Arc::new(Mutex::new(writer));

        // Reader task — blocking I/O on the PTY master.
        let tx_for_reader = broadcast_tx.clone();
        let inner_for_reader = self.inner.clone();
        let id_for_reader = id.clone();
        tokio::task::spawn_blocking(move || {
            reader_loop(
                reader,
                log_file,
                tx_for_reader,
                inner_for_reader,
                id_for_reader,
            );
        });

        // Waiter task — blocking wait() on the child.
        let inner_for_waiter = self.inner.clone();
        let notify_for_waiter = notify_exit.clone();
        let id_for_waiter = id.clone();
        tokio::task::spawn_blocking(move || {
            let mut child = child;
            let code = match child.wait() {
                Ok(status) => status.exit_code() as i32,
                Err(_) => -1,
            };
            on_exit(&id_for_waiter, code);
            {
                let mut inner = inner_for_waiter.lock().unwrap();
                inner.running.remove(&id_for_waiter);
                inner.last_exit.insert(id_for_waiter.clone(), code);
                // The forward task ends on its own (broadcast sender dropped
                // with RunningApp), but the AbortHandle entry would leak
                // across start/stop cycles. Clear it here.
                if let Some(h) = inner.attachments.remove(&id_for_waiter) {
                    h.abort();
                }
            }
            notify_for_waiter.notify_one();
        });

        // Insert into the map and release the starting claim atomically.
        // Reset the scrollback ring buffer here: a fresh run starts with an
        // empty buffer so the user never sees stale output from a prior run.
        // Stash a broadcast sender clone before the move so we can subscribe
        // a probe receiver below without needing to re-lock.
        let bcast_for_probe = broadcast_tx.clone();
        let notify_for_probe = notify_exit.clone();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running.insert(
                id.clone(),
                RunningApp {
                    pid,
                    broadcast_tx,
                    pty_writer,
                    master,
                    notify_exit,
                    port,
                },
            );
            // A fresh run clears stale last_exit.
            inner.last_exit.remove(&id);
            inner.starting.remove(&id);
            inner
                .recent_output
                .insert(id.clone(), RingBuffer::new(RING_CAP_LINES, RING_CAP_BYTES));
            // Probe-less apps go ready immediately; probed apps reset to false.
            inner.ready.insert(id.clone(), probe.is_none());
        }
        guard.armed = false; // already released above

        // Kick off the probe AFTER insert so subscribe-via-Inner works. For
        // probe-less apps we synthesize a ready=true so the frontend gets a
        // single consistent signal regardless of config. Shells are skipped
        // entirely — no consumer cares about their readiness.
        let is_shell = id.starts_with(SHELL_ID_PREFIX);
        if !is_shell {
            if let Some(probe) = probe {
                let inner_for_probe = self.inner.clone();
                let id_for_probe = id.clone();
                let ready_tx = self.ready_broadcast.clone();
                let app_clone = app_for_ready.clone();
                tauri::async_runtime::spawn(async move {
                    let (ready, reason) =
                        run_probe(probe, bcast_for_probe, notify_for_probe).await;
                    {
                        let mut g = inner_for_probe.lock().unwrap();
                        // Only record if this run is still the current one.
                        if g.running.contains_key(&id_for_probe) {
                            g.ready.insert(id_for_probe.clone(), ready);
                        }
                    }
                    // Two channels intentionally: WebKit via Tauri event,
                    // Rust (start_all) via in-process broadcast.
                    let payload = ReadyPayload {
                        id: id_for_probe.clone(),
                        ready,
                        reason,
                    };
                    if let Some(app) = &app_clone {
                        let _ = app.emit("app-ready", payload.clone());
                    }
                    let _ = ready_tx.send(payload);
                });
            } else {
                self.emit_ready(&id, true, None, &app_for_ready);
            }
        }

        Ok(pid)
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        let (pid, notify, port) = {
            let inner = self.inner.lock().unwrap();
            match inner.running.get(id) {
                Some(app) => (app.pid, app.notify_exit.clone(), app.port),
                None => return Ok(()), // idempotent
            }
        };

        // We re-poke SIGTERM on a short interval for up to 5s. Two reasons:
        // (1) interactive zsh ignores SIGTERM at the C level while it's
        //     sourcing .zshrc — if we signal once during that window the
        //     signal is dropped (ignored signals aren't queued), so once zsh
        //     exec's away we need to re-send.
        // (2) the user command may not have spawned children yet at first
        //     stop time; later children become visible to the tree walk.
        // Unix ignored signals aren't queued, so re-poking is the only way.
        let exited = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut exited = false;
            while std::time::Instant::now() < deadline {
                #[cfg(unix)]
                signal_tree(pid, Signal::SIGTERM);
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let tick = std::cmp::min(remaining, Duration::from_millis(200));
                if tick.is_zero() {
                    break;
                }
                let n = notify.notified();
                if tokio::time::timeout(tick, n).await.is_ok() {
                    exited = true;
                    break;
                }
            }
            exited
        };

        if exited {
            // Safety net: orphaned children sometimes outlive the pgroup walk
            // and keep the port held. Sweep it.
            #[cfg(unix)]
            if let Some(p) = port {
                let _ = sweep_port(p).await;
            }
            return Ok(());
        }

        // Grace expired — escalate to SIGKILL.
        #[cfg(unix)]
        signal_tree(pid, Signal::SIGKILL);
        let _ = tokio::time::timeout(Duration::from_secs(1), notify.notified()).await;
        #[cfg(unix)]
        if let Some(p) = port {
            let _ = sweep_port(p).await;
        }
        Ok(())
    }

    pub async fn status(&self, id: &str) -> StatusSnapshot {
        let inner = self.inner.lock().unwrap();
        let ready = inner.ready.get(id).copied().unwrap_or(false);
        if let Some(app) = inner.running.get(id) {
            StatusSnapshot {
                running: true,
                pid: Some(app.pid),
                last_exit: inner.last_exit.get(id).copied(),
                ready,
            }
        } else {
            StatusSnapshot {
                running: false,
                pid: None,
                last_exit: inner.last_exit.get(id).copied(),
                ready: false,
            }
        }
    }

    pub async fn subscribe(&self, id: &str) -> Option<broadcast::Receiver<Vec<u8>>> {
        let inner = self.inner.lock().unwrap();
        inner.running.get(id).map(|a| a.broadcast_tx.subscribe())
    }

    /// Begin forwarding the app's PTY output to a Tauri event named
    /// `pty:<id>:data`, with payload = base64-encoded bytes (so arbitrary
    /// ANSI/binary bytes survive the JSON boundary).
    ///
    /// Idempotent: a second attach for the same id aborts the previous task
    /// before installing a new one.
    ///
    /// If a scrollback snapshot exists for this id (process is still running
    /// OR has exited but never had its ring cleared), it is emitted as a
    /// single `pty:<id>:data` event BEFORE the live forward loop starts.
    /// This lets the UI replay recent output when the user re-focuses an app.
    pub async fn attach(&self, id: &str, app: tauri::AppHandle) -> Result<()> {
        // Snapshot the ring first so the replay always lands before any live
        // chunks. Must complete before installing the forward task.
        let snapshot = {
            let inner = self.inner.lock().unwrap();
            inner
                .recent_output
                .get(id)
                .map(|b| b.snapshot())
                .unwrap_or_default()
        };
        if !snapshot.is_empty() {
            let engine = base64::engine::general_purpose::STANDARD;
            let payload = engine.encode(&snapshot);
            let _ = app.emit(&format!("pty:{id}:data"), payload);
        }

        // No live process? The replay-only path is still useful (you just
        // viewed a crashed app's tail). Skip installing a forward loop.
        let rx = match self.subscribe(id).await {
            Some(rx) => rx,
            None => {
                // Make sure any stale attachment entry is cleared.
                let mut inner = self.inner.lock().unwrap();
                if let Some(h) = inner.attachments.remove(id) {
                    h.abort();
                }
                return Ok(());
            }
        };

        // Abort any prior attachment for this id before installing a new one.
        if let Some(prev) = {
            let mut inner = self.inner.lock().unwrap();
            inner.attachments.remove(id)
        } {
            prev.abort();
        }

        let event_name = format!("pty:{id}:data");
        let task = tokio::spawn(forward_loop(rx, app, event_name));
        let abort = task.abort_handle();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.attachments.insert(id.to_string(), abort);
        }
        Ok(())
    }

    /// Remove the scrollback buffer for an app. Called when the app entry is
    /// deleted so the ring doesn't outlive its owner. Also drops any ready
    /// flag for the same reason.
    pub fn clear_ring(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.recent_output.remove(id);
        inner.ready.remove(id);
    }

    /// Remove an app's on-disk PTY logs (live `<id>.log` + rotated `<id>.log.1`).
    /// Called when the app entry is deleted or a one-off shell is closed so logs
    /// don't outlive their owner. Best-effort: a missing file is not an error.
    pub fn delete_logs(&self, id: &str) {
        let live = self.log_dir.join(format!("{id}.log"));
        let _ = std::fs::remove_file(&live);
        let _ = std::fs::remove_file(live.with_file_name(format!("{id}.log.1")));
    }

    /// Test/diagnostic helper: return the current ring snapshot for an id,
    /// or an empty Vec if no buffer exists.
    #[cfg(test)]
    pub fn recent_snapshot(&self, id: &str) -> Vec<u8> {
        let inner = self.inner.lock().unwrap();
        inner
            .recent_output
            .get(id)
            .map(|b| b.snapshot())
            .unwrap_or_default()
    }

    /// Stop forwarding PTY bytes for this id. No-op if not attached.
    pub async fn detach(&self, id: &str) -> Result<()> {
        let prev = {
            let mut inner = self.inner.lock().unwrap();
            inner.attachments.remove(id)
        };
        if let Some(h) = prev {
            h.abort();
        }
        Ok(())
    }

    pub async fn write_pty(&self, id: &str, bytes: &[u8]) -> Result<()> {
        let writer_arc = {
            let inner = self.inner.lock().unwrap();
            inner.running.get(id).map(|a| a.pty_writer.clone())
        }
        .ok_or_else(|| anyhow!("app {id} not running"))?;
        let mut writer = writer_arc.lock().unwrap();
        writer.write_all(bytes).context("writing to pty")?;
        writer.flush().ok();
        Ok(())
    }

    pub async fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<()> {
        let master_arc = {
            let inner = self.inner.lock().unwrap();
            inner.running.get(id).map(|a| a.master.clone())
        }
        .ok_or_else(|| anyhow!("app {id} not running"))?;
        let master = master_arc.lock().unwrap();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize pty")?;
        Ok(())
    }

    pub async fn stop_all(&self) -> Result<()> {
        let ids: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner.running.keys().cloned().collect()
        };
        for id in ids {
            let _ = self.stop(&id).await;
        }
        Ok(())
    }
}

/// Run a single probe to completion or timeout. Cancelled early if the
/// process exits (notify_exit fires) — in that case we report ready=false
/// with reason="exited" so the frontend stops waiting on amber.
async fn run_probe(
    probe: ReadyProbe,
    broadcast_tx: broadcast::Sender<Vec<u8>>,
    notify_exit: Arc<Notify>,
) -> (bool, Option<String>) {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let work = async move {
        match probe {
            ReadyProbe::Tcp { port } => probe_tcp_loop(port, deadline).await,
            ReadyProbe::Http { url, expect_status } => {
                probe_http_loop(url, expect_status, deadline).await
            }
            ReadyProbe::LogRegex { pattern } => {
                // Subscribe lazily — tcp/http never need a receiver, and the
                // subscription is the only thing that consumes a broadcast slot.
                let rx = broadcast_tx.subscribe();
                probe_log_regex_loop(pattern, rx, deadline).await
            }
        }
    };
    tokio::select! {
        r = work => r,
        _ = notify_exit.notified() => (false, Some("exited".into())),
    }
}

async fn probe_tcp_loop(port: u16, deadline: Instant) -> (bool, Option<String>) {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    loop {
        if Instant::now() >= deadline {
            return (false, Some("timeout".into()));
        }
        let attempt = tokio::time::timeout(
            PROBE_ATTEMPT_TIMEOUT,
            tokio::net::TcpStream::connect(addr),
        )
        .await;
        if matches!(attempt, Ok(Ok(_))) {
            return (true, None);
        }
        tokio::time::sleep(PROBE_POLL_INTERVAL).await;
    }
}

async fn probe_http_loop(
    url: String,
    expect_status: Option<u16>,
    deadline: Instant,
) -> (bool, Option<String>) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(PROBE_ATTEMPT_TIMEOUT)
        .build()
    else {
        return (false, Some("client-build-failed".into()));
    };
    loop {
        if Instant::now() >= deadline {
            return (false, Some("timeout".into()));
        }
        if let Ok(resp) = client.get(&url).send().await {
            let status = resp.status().as_u16();
            let ok = match expect_status {
                Some(want) => status == want,
                // Default: any 2xx/3xx counts as ready.
                None => (200..400).contains(&status),
            };
            if ok {
                return (true, None);
            }
        }
        tokio::time::sleep(PROBE_POLL_INTERVAL).await;
    }
}

async fn probe_log_regex_loop(
    pattern: String,
    mut rx: broadcast::Receiver<Vec<u8>>,
    deadline: Instant,
) -> (bool, Option<String>) {
    // Compile here so an invalid pattern saved by an older client (or by
    // hand-editing apps.json) reports the failure rather than panicking.
    let re = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return (false, Some("invalid-pattern".into())),
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return (false, Some("timeout".into()));
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(bytes)) => {
                let s = String::from_utf8_lossy(&bytes);
                if re.is_match(&s) {
                    return (true, None);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return (false, Some("stream-closed".into()))
            }
            Err(_) => return (false, Some("timeout".into())),
        }
    }
}

/// Kill anything bound to `port` (TCP LISTEN or UDP). SIGTERM, then SIGKILL
/// after a 1s grace. Idempotent and best-effort: missing tools or empty
/// listings are not errors.
#[cfg(unix)]
async fn sweep_port(port: u16) -> Result<()> {
    let initial = pids_on_port(port);
    if initial.is_empty() {
        return Ok(());
    }
    for pid in &initial {
        eprintln!("[sweep_port] SIGTERM pid {pid} on :{port}");
        match kill(Pid::from_raw(*pid as i32), Signal::SIGTERM) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => {} // already gone
            Err(e) => eprintln!("[sweep_port] SIGTERM {pid}: {e}"),
        }
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let survivors = pids_on_port(port);
    for pid in &survivors {
        eprintln!("[sweep_port] SIGKILL pid {pid} on :{port}");
        match kill(Pid::from_raw(*pid as i32), Signal::SIGKILL) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => {}
            Err(e) => eprintln!("[sweep_port] SIGKILL {pid}: {e}"),
        }
    }
    Ok(())
}

/// Returns the set of PIDs currently bound to `port` (TCP listeners and UDP).
/// Empty on missing `lsof` (logs a one-shot warning) or empty listing.
#[cfg(unix)]
fn pids_on_port(port: u16) -> std::collections::HashSet<u32> {
    use std::collections::HashSet;
    let mut out: HashSet<u32> = HashSet::new();
    for spec in [format!("-iTCP:{port}"), format!("-iUDP:{port}")] {
        let mut args = vec!["-nP", "-t"];
        args.push(&spec);
        // For TCP narrow to LISTEN; UDP has no state to filter.
        if spec.starts_with("-iTCP") {
            args.push("-sTCP:LISTEN");
        }
        match std::process::Command::new("lsof").args(&args).output() {
            Ok(o) => {
                if let Ok(s) = String::from_utf8(o.stdout) {
                    for line in s.lines() {
                        if let Ok(p) = line.trim().parse::<u32>() {
                            out.insert(p);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[sweep_port] lsof not available ({e}); skipping port :{port} sweep");
                return HashSet::new();
            }
        }
    }
    out
}

/// Collect `root` and all transitive descendants reachable through the
/// `by_parent` map. Pure function so the stats sampler logic is testable
/// without spawning real processes.
fn descendants_set(
    root: SysPid,
    by_parent: &HashMap<SysPid, Vec<SysPid>>,
) -> HashSet<SysPid> {
    let mut out: HashSet<SysPid> = HashSet::new();
    out.insert(root);
    let mut frontier: Vec<SysPid> = vec![root];
    while let Some(p) = frontier.pop() {
        if let Some(kids) = by_parent.get(&p) {
            for k in kids {
                if out.insert(*k) {
                    frontier.push(*k);
                }
            }
        }
    }
    out
}

#[cfg(unix)]
fn signal_tree(root: u32, sig: Signal) {
    let pids = descendants(root);
    for p in &pids {
        let _ = kill(Pid::from_raw(*p as i32), sig);
    }
    // Also try killpg as a fallback in case the leader is a pgrp leader and
    // there are members we missed via the pgrep walk (e.g. just-spawned).
    let _ = killpg(Pid::from_raw(root as i32), sig);
}

#[cfg(unix)]
fn descendants(root: u32) -> Vec<u32> {
    let mut all = vec![root];
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        if let Ok(out) = std::process::Command::new("pgrep")
            .args(["-P", &p.to_string()])
            .output()
        {
            if let Ok(s) = String::from_utf8(out.stdout) {
                for k in s.split_whitespace() {
                    if let Ok(kid) = k.parse::<u32>() {
                        all.push(kid);
                        frontier.push(kid);
                    }
                }
            }
        }
    }
    all
}

async fn forward_loop(
    mut rx: broadcast::Receiver<Vec<u8>>,
    app: tauri::AppHandle,
    event_name: String,
) {
    let engine = base64::engine::general_purpose::STANDARD;
    loop {
        match rx.recv().await {
            Ok(bytes) => {
                let payload = engine.encode(&bytes);
                let _ = app.emit(&event_name, payload);
            }
            // Sender dropped (process exited): exit cleanly so the abort
            // handle's drop is harmless.
            Err(broadcast::error::RecvError::Closed) => break,
            // Slow receiver — drop the missed messages and keep going.
            // The log file is the durable record of full output.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}

fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    mut log: RotatingLog,
    tx: broadcast::Sender<Vec<u8>>,
    inner: Arc<Mutex<Inner>>,
    id: String,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF: child closed the PTY
            Ok(n) => {
                let _ = log.write(&buf[..n]);
                // Push into the per-app scrollback ring. Critical section is
                // tiny: just a HashMap lookup + VecDeque ops. No .await held.
                {
                    if let Ok(mut g) = inner.lock() {
                        if let Some(ring) = g.recent_output.get_mut(&id) {
                            ring.push(&buf[..n]);
                        }
                    }
                }
                let _ = tx.send(buf[..n].to_vec());
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tempfile::tempdir;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn test_entry(id: &str, command: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            directory: "/tmp".to_string(),
            command: command.to_string(),
            tag: "#000000".to_string(),
            port: None,
            ready: None,
            depends_on: Vec::new(),
        }
    }

    fn test_entry_with_probe(id: &str, command: &str, probe: ReadyProbe) -> AppEntry {
        let mut e = test_entry(id, command);
        e.ready = Some(probe);
        e
    }

    fn test_entry_with_port(id: &str, command: &str, port: u16) -> AppEntry {
        let mut e = test_entry(id, command);
        e.port = Some(port);
        e
    }

    /// Spawn an `nc -l <port>` listener in the background and return its child.
    /// Caller is responsible for killing it (or letting the sweep do that).
    /// Uses macOS nc syntax (`-l <port>` listens on that port).
    fn spawn_listener(port: u16) -> std::process::Child {
        std::process::Command::new("nc")
            .args(["-l", &port.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn nc listener")
    }

    fn wait_for_listener(port: u16) -> bool {
        for _ in 0..30 {
            if !pids_on_port(port).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn exit_recorder() -> (ExitCallback, UnboundedReceiver<(String, i32)>) {
        let (tx, rx) = unbounded_channel::<(String, i32)>();
        let cb: ExitCallback = Box::new(move |id, code| {
            let _ = tx.send((id.to_string(), code));
        });
        (cb, rx)
    }

    fn is_alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
            // ps prints a header line even on miss; success+stdout heuristic
            // is unreliable. Fall back to kill(0) which returns ESRCH if dead.
            || {
                #[cfg(unix)]
                {
                    use nix::sys::signal::kill;
                    kill(Pid::from_raw(pid as i32), None).is_ok()
                }
                #[cfg(not(unix))]
                {
                    false
                }
            }
    }

    fn children_of(pid: u32) -> Vec<u32> {
        std::process::Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|x| x.parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_returns_pid_and_process_is_alive() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _rx) = exit_recorder();
        let pid = pm
            .start_with_callback(test_entry("t1", "sleep 30"), cb)
            .await
            .unwrap();
        // Brief wait for the spawn to settle.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(is_alive(pid), "pid {pid} should be alive after start");
        pm.stop("t1").await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!is_alive(pid), "pid {pid} should be dead after stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_clean_terminates_quickly() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        pm.start_with_callback(test_entry("t2", "sleep 30"), cb)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let t = Instant::now();
        pm.stop("t2").await.unwrap();
        let elapsed = t.elapsed();
        assert!(
            elapsed < Duration::from_millis(800),
            "stop took {elapsed:?}, expected <800ms"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_kills_ignoring_term() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        // `exec` replaces zsh with perl in the same pid/pgrp. Perl ignores
        // SIGTERM, so the only way out is the SIGKILL fallback after 5s.
        pm.start_with_callback(
            test_entry("t3", "exec perl -e '$SIG{TERM} = \"IGNORE\"; sleep 30'"),
            cb,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let t = Instant::now();
        pm.stop("t3").await.unwrap();
        let elapsed = t.elapsed();
        assert!(
            elapsed >= Duration::from_secs(4),
            "stop took {elapsed:?}, expected >=4s (grace window)"
        );
        assert!(
            elapsed <= Duration::from_secs(7),
            "stop took {elapsed:?}, expected <=7s"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_kills_child_processes() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        let pid = pm
            .start_with_callback(test_entry("t4", "sleep 60 & wait"), cb)
            .await
            .unwrap();
        // Wait long enough for outer interactive zsh to source .zshrc and
        // exec into the inner zsh which forks `sleep`.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let kids = children_of(pid);
        assert!(
            !kids.is_empty(),
            "expected at least one child of {pid}, got none"
        );
        pm.stop("t4").await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!is_alive(pid), "leader {pid} should be dead");
        for c in &kids {
            assert!(!is_alive(*c), "child {c} should be dead");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exit_event_fires_with_code() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());

        let (cb_ok, mut rx_ok) = exit_recorder();
        pm.start_with_callback(test_entry("ok", "true"), cb_ok)
            .await
            .unwrap();
        let (id, code) = tokio::time::timeout(Duration::from_secs(5), rx_ok.recv())
            .await
            .expect("timeout waiting for ok exit")
            .expect("recv ok");
        assert_eq!(id, "ok");
        assert_eq!(code, 0);

        let (cb_bad, mut rx_bad) = exit_recorder();
        pm.start_with_callback(test_entry("bad", "false"), cb_bad)
            .await
            .unwrap();
        let (id2, code2) = tokio::time::timeout(Duration::from_secs(5), rx_bad.recv())
            .await
            .expect("timeout waiting for bad exit")
            .expect("recv bad");
        assert_eq!(id2, "bad");
        assert_eq!(code2, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_receives_output() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        pm.start_with_callback(
            test_entry(
                "tick",
                "while :; do echo tick; sleep 0.1; done",
            ),
            cb,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut rx = pm.subscribe("tick").await.expect("subscribe");
        let mut combined = String::new();
        for _ in 0..50 {
            match tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
                Ok(Ok(bytes)) => {
                    combined.push_str(&String::from_utf8_lossy(&bytes));
                    if combined.contains("tick") {
                        break;
                    }
                }
                _ => break,
            }
        }
        pm.stop("tick").await.unwrap();
        assert!(
            combined.contains("tick"),
            "expected 'tick' in subscribed output, got: {combined:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_start_with_same_id_is_rejected() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb1, _) = exit_recorder();
        pm.start_with_callback(test_entry("dup", "sleep 5"), cb1)
            .await
            .unwrap();
        let (cb2, _) = exit_recorder();
        let err = pm
            .start_with_callback(test_entry("dup", "sleep 5"), cb2)
            .await
            .err()
            .expect("second start should error");
        assert!(
            err.to_string().contains("already running"),
            "unexpected error: {err}"
        );
        pm.stop("dup").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_starts_with_same_id_only_one_wins() {
        // Exercises the StartGuard race fix: two start() calls fired at once,
        // exactly one should succeed and the other should error. Without the
        // `starting` set, both can pass the dup check and the second's spawn
        // leaks because it overwrites the first RunningApp in the map.
        let dir = tempdir().unwrap();
        let pm = Arc::new(ProcessManager::new(dir.path().to_path_buf()));
        let pm1 = pm.clone();
        let pm2 = pm.clone();
        let (cb1, _) = exit_recorder();
        let (cb2, _) = exit_recorder();
        let h1 = tokio::spawn(async move {
            pm1.start_with_callback(test_entry("race", "sleep 5"), cb1)
                .await
        });
        let h2 = tokio::spawn(async move {
            pm2.start_with_callback(test_entry("race", "sleep 5"), cb2)
                .await
        });
        let (r1, r2) = tokio::join!(h1, h2);
        let r1 = r1.unwrap();
        let r2 = r2.unwrap();
        let oks = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            oks, 1,
            "exactly one start should succeed, got {oks}: {r1:?} / {r2:?}"
        );
        pm.stop("race").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detach_unknown_id_is_ok() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        pm.detach("ghost").await.unwrap();
        let inner = pm.inner.lock().unwrap();
        assert!(inner.attachments.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiter_clears_attachment_entry_on_exit() {
        // When a process exits, the waiter task must drop any attachment
        // entry so the map doesn't grow unbounded across start/stop cycles.
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, mut rx) = exit_recorder();
        // Plant a fake attachment entry. attach() needs an AppHandle so we
        // can't call it here — just simulate the bookkeeping the same way
        // attach() would.
        {
            let mut inner = pm.inner.lock().unwrap();
            let h = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(30)).await
            })
            .abort_handle();
            inner.attachments.insert("decay".to_string(), h);
        }
        pm.start_with_callback(test_entry("decay", "true"), cb)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let inner = pm.inner.lock().unwrap();
        assert!(
            !inner.attachments.contains_key("decay"),
            "waiter should have cleared the attachment entry"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sweep_port_with_no_listener_is_ok() {
        // Some random high port nothing should be on.
        sweep_port(18901).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sweep_port_kills_listener() {
        let port: u16 = 18765;
        let mut nc = spawn_listener(port);
        assert!(wait_for_listener(port), "nc never bound to :{port}");
        sweep_port(port).await.unwrap();
        // After the sweep, the port must be free.
        let mut free = false;
        for _ in 0..20 {
            if pids_on_port(port).is_empty() {
                free = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = nc.wait();
        assert!(free, "port :{port} still held after sweep");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_runs_preflight_sweep() {
        // Pre-occupy a port with a sidecar nc; then start an entry whose port
        // matches. The start path should sweep the sidecar before spawning,
        // and the port must end free once the entry's quick command exits.
        let port: u16 = 18766;
        let mut nc = spawn_listener(port);
        assert!(wait_for_listener(port), "sidecar nc never bound :{port}");
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, mut rx) = exit_recorder();
        pm.start_with_callback(test_entry_with_port("pre", "true", port), cb)
            .await
            .unwrap();
        // Wait for our command to exit.
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        // Allow waiter cleanup + the post-stop sweep window (n/a here since
        // the process exited on its own). Verify the sidecar is gone.
        let _ = nc.wait();
        let still = pids_on_port(port);
        assert!(
            still.is_empty(),
            "expected port :{port} freed, still held by {:?}",
            still
        );
    }

    #[test]
    fn ring_buffer_trims_to_cap_lines() {
        let mut r = RingBuffer::new(300, 10 * 1024 * 1024);
        for _ in 0..500 {
            r.push(b"x\n");
        }
        let snap = r.snapshot();
        let lines = snap.iter().filter(|&&c| c == b'\n').count();
        assert_eq!(lines, 300, "expected exactly 300 newlines, got {lines}");
    }

    #[test]
    fn ring_buffer_trims_to_cap_bytes() {
        let mut r = RingBuffer::new(usize::MAX, 512 * 1024);
        // One 1MB chunk with no newlines. After push, the byte cap should
        // trim down. Note: with a single oversized chunk and no smaller
        // predecessors, the trim loop pops the only chunk, ending at 0 bytes.
        // That's the expected safety behavior: better empty than unbounded.
        let big = vec![b'a'; 1024 * 1024];
        r.push(&big);
        let snap = r.snapshot();
        assert!(
            snap.len() <= 512 * 1024,
            "snapshot len {} exceeds cap",
            snap.len()
        );
    }

    #[test]
    fn ring_buffer_preserves_recent_bytes_with_byte_cap() {
        // Push many small chunks totaling more than the byte cap; verify the
        // tail is preserved (the newest chunk is intact).
        let mut r = RingBuffer::new(usize::MAX, 1024);
        for i in 0..100u8 {
            r.push(&[i; 64]); // 100 * 64 = 6400 bytes total
        }
        let snap = r.snapshot();
        assert!(snap.len() <= 1024, "snapshot len {}", snap.len());
        // Last chunk should be all 99s.
        assert!(
            snap.iter().rev().take(64).all(|&b| b == 99),
            "tail not preserved"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ring_buffer_resets_on_restart() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());

        let (cb1, mut rx1) = exit_recorder();
        pm.start_with_callback(test_entry("reset", "echo first-run"), cb1)
            .await
            .unwrap();
        // Wait for the first run to fully exit so the reader drains EOF.
        let _ = tokio::time::timeout(Duration::from_secs(5), rx1.recv()).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let snap1 = pm.recent_snapshot("reset");
        assert!(
            String::from_utf8_lossy(&snap1).contains("first-run"),
            "first run not in ring: {:?}",
            String::from_utf8_lossy(&snap1)
        );

        // Second start: ring should be reset before any second-run bytes land.
        let (cb2, mut rx2) = exit_recorder();
        pm.start_with_callback(test_entry("reset", "echo second-run"), cb2)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx2.recv()).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let snap2 = pm.recent_snapshot("reset");
        let s2 = String::from_utf8_lossy(&snap2);
        assert!(s2.contains("second-run"), "second run missing: {s2:?}");
        assert!(
            !s2.contains("first-run"),
            "first-run leaked into ring after restart: {s2:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ring_buffer_survives_process_exit() {
        // After the process exits, the ring must still be readable so the
        // user can scroll back a crashed app's tail.
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, mut rx) = exit_recorder();
        pm.start_with_callback(
            test_entry("survive", "echo last-words; exit 0"),
            cb,
        )
        .await
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let snap = pm.recent_snapshot("survive");
        assert!(
            String::from_utf8_lossy(&snap).contains("last-words"),
            "ring lost after exit: {:?}",
            String::from_utf8_lossy(&snap)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clear_ring_drops_buffer() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, mut rx) = exit_recorder();
        pm.start_with_callback(test_entry("zap", "echo hi"), cb)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!pm.recent_snapshot("zap").is_empty());
        pm.clear_ring("zap");
        assert!(pm.recent_snapshot("zap").is_empty(), "ring not cleared");
    }

    #[test]
    fn shell_id_has_oneoff_prefix_and_ulid_suffix() {
        // ULIDs are 26 chars; the prefix is `oneoff:` (7 chars). This locks
        // the id shape the running_count / stats sampler filters depend on.
        let id = format!("{SHELL_ID_PREFIX}{}", Ulid::new());
        assert!(id.starts_with(SHELL_ID_PREFIX), "id was {id}");
        assert_eq!(id.len(), SHELL_ID_PREFIX.len() + 26, "id was {id}");
    }

    #[test]
    fn descendants_set_collects_full_tree() {
        // Shape:
        //   1
        //   ├── 2
        //   │   └── 4
        //   │       └── 5
        //   └── 3
        //   100  (unrelated)
        let mut by_parent: HashMap<SysPid, Vec<SysPid>> = HashMap::new();
        by_parent.insert(SysPid::from_u32(1), vec![SysPid::from_u32(2), SysPid::from_u32(3)]);
        by_parent.insert(SysPid::from_u32(2), vec![SysPid::from_u32(4)]);
        by_parent.insert(SysPid::from_u32(4), vec![SysPid::from_u32(5)]);
        // pid 100 has its own subtree that must not be included
        by_parent.insert(SysPid::from_u32(99), vec![SysPid::from_u32(100)]);

        let tree = descendants_set(SysPid::from_u32(1), &by_parent);
        let expected: HashSet<SysPid> = [1u32, 2, 3, 4, 5]
            .into_iter()
            .map(SysPid::from_u32)
            .collect();
        assert_eq!(tree, expected);
    }

    #[test]
    fn descendants_set_handles_leaf_with_no_children() {
        let by_parent: HashMap<SysPid, Vec<SysPid>> = HashMap::new();
        let tree = descendants_set(SysPid::from_u32(42), &by_parent);
        assert_eq!(tree.len(), 1);
        assert!(tree.contains(&SysPid::from_u32(42)));
    }

    #[test]
    fn descendants_set_is_cycle_safe() {
        // Defensive: real /proc-ish data shouldn't contain cycles, but we
        // rely on the HashSet insert guard rather than trusting the source.
        let mut by_parent: HashMap<SysPid, Vec<SysPid>> = HashMap::new();
        by_parent.insert(SysPid::from_u32(1), vec![SysPid::from_u32(2)]);
        by_parent.insert(SysPid::from_u32(2), vec![SysPid::from_u32(1)]); // cycle
        let tree = descendants_set(SysPid::from_u32(1), &by_parent);
        let expected: HashSet<SysPid> =
            [1u32, 2].into_iter().map(SysPid::from_u32).collect();
        assert_eq!(tree, expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_none_marks_ready_immediately() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        // Subscribe BEFORE start: probe-less apps fire synchronously inside spawn_pty.
        let mut rrx = pm.subscribe_ready();
        pm.start_with_callback(test_entry("noprobe", "sleep 5"), cb)
            .await
            .unwrap();
        let payload = tokio::time::timeout(Duration::from_secs(2), rrx.recv())
            .await
            .expect("ready event should fire fast")
            .expect("recv");
        assert_eq!(payload.id, "noprobe");
        assert!(payload.ready);
        assert_eq!(payload.reason, None);
        pm.stop("noprobe").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_probe_fires_ready_when_port_bound() {
        let port: u16 = 18891;
        let mut nc = spawn_listener(port);
        assert!(wait_for_listener(port), "nc never bound to :{port}");
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        let mut rrx = pm.subscribe_ready();
        pm.start_with_callback(
            test_entry_with_probe("tcpok", "sleep 5", ReadyProbe::Tcp { port }),
            cb,
        )
        .await
        .unwrap();
        let payload = tokio::time::timeout(Duration::from_secs(4), rrx.recv())
            .await
            .expect("ready event timeout")
            .expect("recv");
        assert_eq!(payload.id, "tcpok");
        assert!(payload.ready, "expected ready=true with listener bound");
        pm.stop("tcpok").await.unwrap();
        let _ = nc.wait();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_probe_times_out_when_port_silent() {
        // PROBE_TIMEOUT is 3s under cfg(test), so this resolves in ~3-4s
        // rather than the production 60s.
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        let mut rrx = pm.subscribe_ready();
        pm.start_with_callback(
            test_entry_with_probe(
                "tcpto",
                "sleep 30",
                ReadyProbe::Tcp { port: 18892 },
            ),
            cb,
        )
        .await
        .unwrap();
        let payload = tokio::time::timeout(Duration::from_secs(8), rrx.recv())
            .await
            .expect("ready event timeout")
            .expect("recv");
        assert_eq!(payload.id, "tcpto");
        assert!(!payload.ready, "expected ready=false on timeout");
        assert_eq!(payload.reason.as_deref(), Some("timeout"));
        // Process still alive even though probe failed.
        assert!(pm.status("tcpto").await.running);
        pm.stop("tcpto").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn log_regex_probe_fires_on_match() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        let mut rrx = pm.subscribe_ready();
        pm.start_with_callback(
            test_entry_with_probe(
                "logmatch",
                "echo 'now listening on :8080'; sleep 5",
                ReadyProbe::LogRegex { pattern: "listening on".into() },
            ),
            cb,
        )
        .await
        .unwrap();
        let payload = tokio::time::timeout(Duration::from_secs(4), rrx.recv())
            .await
            .expect("ready event timeout")
            .expect("recv");
        assert_eq!(payload.id, "logmatch");
        assert!(payload.ready);
        pm.stop("logmatch").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_reflects_ready_flag() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, _) = exit_recorder();
        let mut rrx = pm.subscribe_ready();
        pm.start_with_callback(test_entry("statusprobe", "sleep 5"), cb)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), rrx.recv()).await;
        let s = pm.status("statusprobe").await;
        assert!(s.running);
        assert!(s.ready, "probe-less app should be ready");
        pm.stop("statusprobe").await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn log_file_grows() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        let (cb, mut rx) = exit_recorder();
        pm.start_with_callback(test_entry("logme", "echo hello-from-test"), cb)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let log = std::fs::read_to_string(dir.path().join("logme.log")).unwrap();
        assert!(
            log.contains("hello-from-test"),
            "log content: {log:?}"
        );
    }

    #[test]
    fn rotating_log_caps_live_file_and_keeps_one_generation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cap.log");
        let mut log = RotatingLog::open(path.clone()).unwrap();

        // Write comfortably past the cap so at least one rotation must occur.
        let chunk = vec![b'x'; 64 * 1024];
        let mut written = 0u64;
        while written <= MAX_LOG_BYTES + 128 * 1024 {
            log.write(&chunk).unwrap();
            written += chunk.len() as u64;
        }

        let live = std::fs::metadata(&path).unwrap().len();
        assert!(live <= MAX_LOG_BYTES, "live log {live} exceeds cap");
        assert!(
            dir.path().join("cap.log.1").exists(),
            "expected one rotated generation"
        );
    }

    #[test]
    fn delete_logs_removes_live_and_rotated() {
        let dir = tempdir().unwrap();
        let pm = ProcessManager::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("gone.log"), b"live").unwrap();
        std::fs::write(dir.path().join("gone.log.1"), b"rotated").unwrap();

        pm.delete_logs("gone");

        assert!(!dir.path().join("gone.log").exists());
        assert!(!dir.path().join("gone.log.1").exists());
    }
}
