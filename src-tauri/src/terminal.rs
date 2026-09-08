//! Per-conversation terminal sessions: a real PTY the user drives from the
//! chat view, entirely separate from the agent. One shell per conversation,
//! kept alive while the app runs; output is streamed to the webview as
//! `TerminalOutput` events and buffered so a reopened panel replays history.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::events::{BackendEvent, EventSink};

/// How much terminal output to keep for replay when the panel reopens.
const SCROLLBACK_LIMIT: usize = 1024 * 1024;
const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

/// All live terminal sessions, keyed by conversation id. Shared with the
/// waiter threads so an exiting shell can remove its own entry.
pub type TerminalMap = Arc<Mutex<HashMap<String, Arc<TerminalSession>>>>;

type ChildBox = Box<dyn Child + Send + Sync>;
type ReaderBox = Box<dyn Read + Send>;

/// Capped byte buffer of recent PTY output, replayed on reattach. The chunk
/// counter lives under the same lock as the bytes, so a snapshot always knows
/// exactly how many emitted chunks it already contains.
struct Scrollback(Mutex<ScrollbackInner>);

struct ScrollbackInner {
    bytes: Vec<u8>,
    chunks: u64,
}

impl Scrollback {
    /// Append one chunk and return its 0-based sequence number.
    fn push(&self, bytes: &[u8]) -> u64 {
        let mut inner = self.0.lock().unwrap();
        inner.bytes.extend_from_slice(bytes);
        let excess = inner.bytes.len().saturating_sub(SCROLLBACK_LIMIT);
        if excess > 0 {
            inner.bytes.drain(0..excess);
        }
        let seq = inner.chunks;
        inner.chunks += 1;
        seq
    }

    /// The buffered bytes plus the number of chunks they contain: live events
    /// with `seq >=` that count are NOT in the snapshot and must still be written.
    fn snapshot(&self) -> (Vec<u8>, u64) {
        let inner = self.0.lock().unwrap();
        (inner.bytes.clone(), inner.chunks)
    }
}

pub struct TerminalSession {
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    scrollback: Arc<Scrollback>,
    /// Set once the shell has been reaped; a dead session gets replaced, not
    /// reused, on the next `get_or_spawn`.
    dead: AtomicBool,
}

impl TerminalSession {
    pub fn write(&self, data: &str) -> Result<(), String> {
        self.writer
            .lock()
            .unwrap()
            .write_all(data.as_bytes())
            .map_err(|e| format!("terminal write failed: {e}"))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("terminal resize failed: {e}"))
    }

    /// Kill the shell; the waiter thread reaps it and tears the session down.
    pub fn kill(&self) {
        let _ = self.killer.lock().unwrap().kill();
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Buffered output so far plus how many chunks it contains: live events
    /// with `seq >=` that count are NOT in the bytes and must still be written.
    pub fn snapshot(&self) -> (Vec<u8>, u64) {
        self.scrollback.snapshot()
    }
}

fn resolve_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    }
}

/// Return the live session for a conversation, spawning a fresh shell if
/// none is running. Safe to call concurrently: a lost race gets the winner's
/// session back and reaps its spare.
pub fn get_or_spawn(
    conversation_id: &str,
    cwd: &Path,
    sink: Arc<dyn EventSink>,
    terminals: TerminalMap,
) -> Result<Arc<TerminalSession>, String> {
    {
        let map = terminals.lock().unwrap();
        if let Some(existing) = map.get(conversation_id) {
            if !existing.is_dead() {
                return Ok(existing.clone());
            }
        }
    }

    let (session, mut child, reader) = spawn_session(conversation_id, cwd)?;

    let mut map = terminals.lock().unwrap();
    if let Some(existing) = map.get(conversation_id).cloned() {
        if !existing.is_dead() {
            drop(map);
            session.kill();
            // The child was just killed, so this returns promptly.
            let _ = child.wait();
            return Ok(existing);
        }
    }
    map.insert(conversation_id.to_string(), session.clone());
    drop(map);

    start_reader_thread(conversation_id, reader, session.clone(), sink.clone());
    start_waiter_thread(conversation_id, session.clone(), child, sink, terminals);
    Ok(session)
}

fn spawn_session(
    conversation_id: &str,
    cwd: &Path,
) -> Result<(Arc<TerminalSession>, ChildBox, ReaderBox), String> {
    let shell = resolve_shell();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open a pty: {e}"))?;

    let mut cmd = CommandBuilder::new(&shell);
    #[cfg(unix)]
    cmd.arg("-l"); // login shell: picks up the user's profile/PATH
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("DUCKY_CONVERSATION", conversation_id);

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn {shell}: {e}"))?;
    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to read from the pty: {e}"));
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to write to the pty: {e}"));
        }
    };
    let killer = child.clone_killer();
    // Drop our slave handle so the master sees EOF once the child exits.
    drop(pair.slave);

    let session = Arc::new(TerminalSession {
        writer: Mutex::new(writer),
        killer: Mutex::new(killer),
        master: Mutex::new(pair.master),
        scrollback: Arc::new(Scrollback(Mutex::new(ScrollbackInner {
            bytes: Vec::new(),
            chunks: 0,
        }))),
        dead: AtomicBool::new(false),
    });
    Ok((session, child, reader))
}

/// Continuously read PTY output into the scrollback buffer and stream it to
/// the webview. Exits when the master hits EOF (shell exited) or errors.
fn start_reader_thread(
    conversation_id: &str,
    mut reader: ReaderBox,
    session: Arc<TerminalSession>,
    sink: Arc<dyn EventSink>,
) {
    let conversation_id = conversation_id.to_string();
    // Blocking IO: a plain thread keeps the tokio runtime untouched.
    let _ = std::thread::Builder::new()
        .name(format!("term-read-{conversation_id}"))
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = &chunk[..n];
                        let seq = session.scrollback.push(data);
                        sink.emit(BackendEvent::TerminalOutput {
                            conversation_id: conversation_id.clone(),
                            data: base64::engine::general_purpose::STANDARD.encode(data),
                            seq,
                        });
                    }
                    Err(_) => break,
                }
            }
        });
}

/// Reap the shell when it exits, remove the session, and tell the webview.
fn start_waiter_thread(
    conversation_id: &str,
    session: Arc<TerminalSession>,
    mut child: ChildBox,
    sink: Arc<dyn EventSink>,
    terminals: TerminalMap,
) {
    let conversation_id = conversation_id.to_string();
    let _ = std::thread::Builder::new()
        .name(format!("term-wait-{conversation_id}"))
        .spawn(move || {
            let code = child
                .wait()
                .ok()
                .and_then(|status| i32::try_from(status.exit_code()).ok());
            session.dead.store(true, Ordering::SeqCst);
            let mut map = terminals.lock().unwrap();
            let is_current = map
                .get(&conversation_id)
                .map(|current| Arc::ptr_eq(current, &session))
                .unwrap_or(false);
            if is_current {
                map.remove(&conversation_id);
            }
            drop(map);
            sink.emit(BackendEvent::TerminalClosed {
                conversation_id,
                exit_code: code,
            });
        });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::events::CollectingSink;
    use std::time::{Duration, Instant};

    #[test]
    fn pty_roundtrip_streams_and_buffers() {
        let sink = Arc::new(CollectingSink::default());
        let terminals: TerminalMap = Arc::new(Mutex::new(HashMap::new()));
        let session = get_or_spawn(
            "test-conv-roundtrip",
            &std::env::temp_dir(),
            sink.clone(),
            terminals,
        )
        .expect("spawn terminal");

        session
            .write("echo ducky_pty_roundtrip_marker\r")
            .expect("write to pty");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while Instant::now() < deadline {
            seen = String::from_utf8_lossy(&session.snapshot().0).into_owned();
            if seen.contains("ducky_pty_roundtrip_marker") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            seen.contains("ducky_pty_roundtrip_marker"),
            "scrollback never contained the marker: {seen:?}"
        );

        let events = sink.events.lock().unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, BackendEvent::TerminalOutput { .. })));

        session.kill();
    }

    #[test]
    fn get_or_spawn_is_idempotent() {
        let sink = Arc::new(CollectingSink::default());
        let terminals: TerminalMap = Arc::new(Mutex::new(HashMap::new()));
        let cwd = std::env::temp_dir();
        let first =
            get_or_spawn("test-conv-idem", &cwd, sink.clone(), terminals.clone()).expect("first");
        let second =
            get_or_spawn("test-conv-idem", &cwd, sink.clone(), terminals.clone()).expect("second");
        assert!(Arc::ptr_eq(&first, &second));

        first.kill();
        first.dead.store(true, Ordering::SeqCst);
        let third = get_or_spawn("test-conv-idem", &cwd, sink, terminals).expect("third");
        assert!(!Arc::ptr_eq(&first, &third));
        third.kill();
    }
}
