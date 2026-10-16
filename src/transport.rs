//! Content-Length-framed JSON over a child's stdio — the wire shared by the
//! LSP client ([`crate::lsp`]) and the DAP transport ([`crate::dap`]).
//!
//! [`spawn_stdio`] starts the server (piped stdio, its own process group, the
//! caller's env overrides) with the error reporting both protocols want;
//! [`frame`] / [`spawn_writer`] queue outgoing messages off the UI thread;
//! [`read_frames`] and [`for_each_stderr_line`] run on reader threads.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use futures::channel::mpsc::UnboundedSender;
use serde_json::{Value, json};

use crate::process::{EnvOverrides, ProcessGroup};
use crate::util::HideConsole;

/// Why [`spawn_stdio`] failed. `Display` is the human-readable reason the
/// editor shows (and logs).
#[derive(Debug)]
pub enum SpawnError {
    /// The working directory is missing — otherwise `spawn` would fail with
    /// `NotFound`, indistinguishable from a missing executable.
    MissingCwd(std::path::PathBuf),
    /// The program isn't on `PATH` (the usual GUI-launch failure mode).
    NotFound { program: String, path: String },
    /// Any other spawn failure.
    Spawn {
        program: String,
        error: std::io::Error,
    },
    /// The child came up without its piped stdio (can't happen in practice).
    NoPipes,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::MissingCwd(cwd) => write!(
                f,
                "working directory `{}` does not exist (or is not a directory)",
                cwd.display()
            ),
            SpawnError::NotFound { program, path } => write!(
                f,
                "`{program}` not found in PATH. \
                 If it's installed via nvm/pyenv/cargo/brew, the GUI launch may not see it — \
                 try (setenv \"PATH\" …) in init.scm. PATH={path}"
            ),
            SpawnError::Spawn { program, error } => write!(f, "spawning `{program}`: {error}"),
            SpawnError::NoPipes => f.write_str("child has no stdio pipes"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// A spawned server: its process group plus its three pipes.
pub struct StdioTransport {
    pub group: ProcessGroup,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// A spawned server's working directory must exist: otherwise `spawn` fails
/// with `NotFound`, indistinguishable from a missing executable. An empty
/// path means "inherit ours" and is fine.
pub fn check_spawn_cwd(cwd: &Path) -> Result<(), SpawnError> {
    if cwd.as_os_str().is_empty() || cwd.is_dir() {
        Ok(())
    } else {
        Err(SpawnError::MissingCwd(cwd.to_path_buf()))
    }
}

/// Spawn `program args…` in `cwd` with piped stdio, `env` applied on top of
/// the inherited environment, and its own process group — so dropping the
/// group takes down whatever a wrapper spawned too (mirrors `make-process`).
pub fn spawn_stdio(
    program: &str,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    cwd: &Path,
    env: &EnvOverrides,
) -> Result<StdioTransport, SpawnError> {
    check_spawn_cwd(cwd)?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .hide_console();
    env.apply(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SpawnError::NotFound {
                program: program.to_string(),
                path: crate::process::getenv("PATH").unwrap_or_default(),
            });
        }
        Err(error) => {
            return Err(SpawnError::Spawn {
                program: program.to_string(),
                error,
            });
        }
    };
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    // Dropping the group on the error path kills and reaps the child.
    let group = ProcessGroup::new(child);
    let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
        return Err(SpawnError::NoPipes);
    };
    Ok(StdioTransport {
        group,
        stdin,
        stdout,
        stderr,
    })
}

/// Frame one serialized message for the wire.
pub fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// Write queued frames to `writer` until the queue closes or the pipe breaks.
pub fn write_frames(frames: Receiver<String>, mut writer: impl Write) {
    while let Ok(frame) = frames.recv() {
        if writer.write_all(frame.as_bytes()).is_err() {
            break; // pipe closed
        }
        let _ = writer.flush();
    }
}

/// A writer thread draining queued frames to `writer` (a child's stdin), so
/// the UI never blocks on a full pipe (e.g. a large `didOpen` to a busy
/// server). Sending fails once the pipe is gone.
pub fn spawn_writer(writer: impl Write + Send + 'static) -> Sender<String> {
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || write_frames(rx, writer));
    tx
}

/// Largest message body we'll allocate for. `Content-Length` comes straight
/// from the server, so a garbage header must not make us allocate gigabytes;
/// anything bigger is treated as a protocol error (the stream is abandoned,
/// which the editor then handles exactly like the server exiting).
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// Read Content-Length-framed JSON messages until EOF, forwarding each. The
/// framing loop, generic over the reader so it's testable with a `Cursor`.
pub fn read_frames<R: BufRead>(mut reader: R, tx: &UnboundedSender<Value>) {
    loop {
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return, // EOF or broken pipe
                Ok(_) => {}
            }
            let line = line.trim_end();
            if line.is_empty() {
                break; // blank line ends the header block
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        if content_length == 0 {
            continue;
        }
        if content_length > MAX_FRAME_BYTES {
            let line = format!("protocol error: Content-Length {content_length} exceeds limit");
            let _ = tx.unbounded_send(json!({ "method": "$/stderr", "params": { "line": line } }));
            return; // the stream is unsynchronised now; treat it as dead
        }
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        match serde_json::from_slice::<Value>(&buf) {
            Ok(value) => {
                if tx.unbounded_send(value).is_err() {
                    return; // editor dropped the receiver
                }
            }
            Err(_) => continue,
        }
    }
}

/// Read `reader` (a child's stderr) line by line until EOF, passing each line
/// (lossily decoded, without the trailing newline) to `f`. Non-UTF-8 output
/// must not end the loop — dropping the pipe would let the server die on
/// SIGPIPE/EPIPE. Once `f` returns `false` (nobody's listening) the pipe is
/// still drained, just without forwarding.
pub fn for_each_stderr_line<R: std::io::Read>(reader: R, mut f: impl FnMut(String) -> bool) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut forwarding = true;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
        if !forwarding {
            continue;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        forwarding = f(String::from_utf8_lossy(&buf).into_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::channel::mpsc::unbounded;
    use std::io::Cursor;

    #[test]
    fn parses_two_framed_messages() {
        let a = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let b = r#"{"method":"textDocument/publishDiagnostics"}"#;
        let stream = format!(
            "Content-Length: {}\r\n\r\n{a}Content-Length: {}\r\n\r\n{b}",
            a.len(),
            b.len()
        );

        let (tx, mut rx) = unbounded();
        read_frames(Cursor::new(stream.into_bytes()), &tx); // runs to EOF
        drop(tx);

        let first = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(first["id"], json!(1));
        let second = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(second["method"], json!("textDocument/publishDiagnostics"));
        assert!(futures::executor::block_on(rx.next()).is_none()); // closed, drained
    }

    #[test]
    fn oversized_frame_is_a_protocol_error_not_an_allocation() {
        // A bogus multi-gigabyte Content-Length must not be allocated; the
        // reader reports it and abandons the stream (= the server is dead).
        let ok = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let stream = format!(
            "Content-Length: 99999999999\r\n\r\n{{}}Content-Length: {}\r\n\r\n{ok}",
            ok.len()
        );
        let (tx, mut rx) = unbounded();
        read_frames(Cursor::new(stream.into_bytes()), &tx);
        drop(tx);
        let report = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(report["method"], json!("$/stderr"));
        assert!(
            report["params"]["line"]
                .as_str()
                .unwrap()
                .contains("exceeds")
        );
        assert!(futures::executor::block_on(rx.next()).is_none()); // nothing after it
    }

    #[test]
    fn stderr_lines_survive_invalid_utf8() {
        let input: &[u8] = b"one\n\xff\xfebad\r\nthree";
        let mut lines = Vec::new();
        for_each_stderr_line(input, |l| {
            lines.push(l);
            true
        });
        assert_eq!(lines, vec!["one", "\u{FFFD}\u{FFFD}bad", "three"]);

        // After the sink stops accepting, the rest is still drained (no panic,
        // no further calls).
        let mut calls = 0;
        for_each_stderr_line(&b"a\nb\nc\n"[..], |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1);
    }
}
