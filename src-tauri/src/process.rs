use crate::config::AppEntry;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{broadcast, Notify};
use tokio::task::AbortHandle;

#[cfg(unix)]
use nix::sys::signal::{kill, killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

/// Called when a child exits. The id is the app id; the i32 is the exit code.
pub type ExitCallback = Box<dyn Fn(&str, i32) + Send + Sync + 'static>;

#[derive(Clone, Debug, Serialize)]
pub struct StatusSnapshot {
    pub running: bool,
    pub pid: Option<u32>,
    pub last_exit: Option<i32>,
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
}

pub struct ProcessManager {
    inner: Arc<Mutex<Inner>>,
    log_dir: PathBuf,
}

impl ProcessManager {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                running: HashMap::new(),
                last_exit: HashMap::new(),
                starting: std::collections::HashSet::new(),
                attachments: HashMap::new(),
            })),
            log_dir,
        }
    }

    /// Number of currently-running apps. Used by the tray to update the
    /// "Running: N" label.
    pub fn running_count(&self) -> usize {
        self.inner.lock().unwrap().running.len()
    }

    /// Production entry point: emit a Tauri event on exit.
    pub async fn start(&self, app: tauri::AppHandle, entry: AppEntry) -> Result<u32> {
        let app_for_cb = app.clone();
        let id_for_event = entry.id.clone();
        let pid = self
            .start_with_callback(
                entry,
                Box::new(move |id, code| {
                    use tauri::Emitter;
                    // Single global event; frontend routes by id.
                    let _ = app_for_cb.emit(
                        "app-exit",
                        ExitPayload {
                            id: id.to_string(),
                            code,
                        },
                    );
                }),
            )
            .await?;
        // Also emit a global app-started event so listeners (e.g. the tray
        // "Running: N" label) can refresh without polling.
        use tauri::Emitter;
        let _ = app.emit(
            "app-started",
            StartedPayload {
                id: id_for_event,
                pid,
            },
        );
        Ok(pid)
    }

    /// Test/internal entry point: callback receives (id, exit_code) on child exit.
    pub async fn start_with_callback(
        &self,
        entry: AppEntry,
        on_exit: ExitCallback,
    ) -> Result<u32> {
        let id = entry.id.clone();

        // Atomic dup-check + claim. Holding `starting` for the duration of
        // spawn prevents a second concurrent start(id) from passing the
        // running-map check and leaking a child by overwriting the first
        // RunningApp on insert. Released on success (after insert) or via
        // the StartGuard's Drop on any error path.
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
        // a previous run that didn't shut down cleanly.
        #[cfg(unix)]
        if let Some(p) = entry.port {
            let _ = sweep_port(p).await;
        }

        std::fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("creating log dir {}", self.log_dir.display()))?;
        let log_path = self.log_dir.join(format!("{id}.log"));
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
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

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawning command in {}", entry.directory))?;
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
        tokio::task::spawn_blocking(move || {
            reader_loop(reader, log_file, tx_for_reader);
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
                    port: entry.port,
                },
            );
            // A fresh run clears stale last_exit.
            inner.last_exit.remove(&id);
            inner.starting.remove(&id);
        }
        guard.armed = false; // already released above

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
        if let Some(app) = inner.running.get(id) {
            StatusSnapshot {
                running: true,
                pid: Some(app.pid),
                last_exit: inner.last_exit.get(id).copied(),
            }
        } else {
            StatusSnapshot {
                running: false,
                pid: None,
                last_exit: inner.last_exit.get(id).copied(),
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
    pub async fn attach(&self, id: &str, app: tauri::AppHandle) -> Result<()> {
        let rx = self
            .subscribe(id)
            .await
            .ok_or_else(|| anyhow!("app {id} not running"))?;

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
    mut log: File,
    tx: broadcast::Sender<Vec<u8>>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF: child closed the PTY
            Ok(n) => {
                let _ = log.write_all(&buf[..n]);
                let _ = log.flush();
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
        }
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
}
