//! `<app> ctl <command>`: a newline-delimited text protocol over a unix socket in
//! `$XDG_RUNTIME_DIR/<name>.sock`. One request per connection, one reply line. This is the
//! IPC every vanta daemon exposes (vantapaper's `ctl` protocol is exactly this).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context, Result};

pub fn socket_path(name: &str) -> PathBuf {
    crate::paths::runtime_dir().join(format!("{name}.sock"))
}

/// A request waiting for the daemon's reply.
pub struct Request {
    pub line: String,
    reply: Sender<String>,
}

impl Request {
    pub fn reply(self, text: impl Into<String>) {
        let _ = self.reply.send(text.into());
    }
}

/// Accepts connections on a background thread and hands each request to the daemon's loop
/// through `poll`. The daemon answers with [`Request::reply`]; a request left unanswered gets
/// "error: no reply" after two seconds so clients never hang.
pub struct Server {
    rx: Receiver<Request>,
    path: PathBuf,
}

impl Server {
    pub fn bind(name: &str) -> Result<Self> {
        Self::bind_with_wake(name, || {})
    }

    /// The same, calling `wake` whenever a request is waiting, so a daemon whose loop sleeps
    /// until something happens can be woken for it instead of polling.
    pub fn bind_with_wake(name: &str, wake: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let wake = std::sync::Arc::new(wake);
        let path = socket_path(name);
        // A stale socket from a crashed instance is harmless to remove; a live one is not,
        // so probe it first.
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                anyhow::bail!("{} is already served by another instance", path.display());
            }
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("ctl-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let tx = tx.clone();
                    let wake = wake.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut line = String::new();
                        let Ok(read) = stream.try_clone() else { return };
                        if BufReader::new(read).read_line(&mut line).is_err() {
                            return;
                        }
                        let line = line.trim().to_string();
                        // A connection that says nothing (another instance probing whether this
                        // one is alive, say) is not a request.
                        if line.is_empty() {
                            return;
                        }
                        let (rtx, rrx) = mpsc::channel::<String>();
                        if tx.send(Request { line, reply: rtx }).is_err() {
                            return;
                        }
                        wake();
                        let reply = rrx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| "error: no reply".into());
                        let _ = writeln!(stream, "{reply}");
                    });
                }
            })
            .context("spawn ctl thread")?;
        Ok(Server { rx, path })
    }

    /// Non-blocking: the next pending request, if any.
    pub fn poll(&self) -> Option<Request> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Sends one command and returns the reply line (client side of `<app> ctl`).
pub fn send(name: &str, command: &str) -> Result<String> {
    let path = socket_path(name);
    let mut stream = UnixStream::connect(&path).with_context(|| format!("connect {} (is {name} running?)", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{command}")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}
