//! Debug Adapter Protocol transport: spawn (or TCP-connect to) a debug
//! adapter and speak Content-Length-framed JSON with it — the same wire
//! format as LSP, so spawning and framing are shared with it via
//! [`crate::transport`].
//!
//! Deliberately protocol-free: no `seq` stamping, no request/response
//! correlation, no capability knowledge. The whole DAP state machine lives in
//! Scheme (`dap.scm`), which builds complete message objects and receives
//! every incoming message verbatim — a new adapter feature needs no Rust.
//!
//! Nothing here blocks the UI thread: spawning is the only synchronous step;
//! reading, writing, TCP connecting, and reaping all happen on worker threads
//! (or, for [`DapClient::finish`], the caller's background executor).

use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use serde_json::{Value, json};

use crate::process::ProcessGroup;
use crate::transport;

/// How long a TCP connect (`dap-connect`) may take before the session is
/// declared dead. The connect runs on the transport's writer thread.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// After the message channel closes, how long to wait for the adapter to exit
/// on its own before its process group is SIGKILLed so it can be reaped.
const REAP_GRACE: Duration = Duration::from_secs(2);

/// State shared between a TCP session's handle and its connector thread.
#[derive(Default)]
struct SocketSlot {
    /// A clone of the live stream, once connected, so `kill` can shut it down.
    stream: Option<TcpStream>,
    /// `kill` arrived before the connect finished: don't bring the session up.
    cancelled: bool,
    /// Why the connect failed, for the exit report.
    error: Option<String>,
}

pub struct DapClient {
    /// The adapter's process group for a stdio session; `None` for a TCP
    /// session (whose server process, if any, Scheme spawns and owns via
    /// `make-process`).
    child: Option<ProcessGroup>,
    /// For TCP sessions, the slot the connector thread fills in, so `kill`
    /// can shut the stream down (the reader and writer threads then unwind on
    /// their own) or cancel a connect still in flight.
    socket: Option<Arc<Mutex<SocketSlot>>>,
    outgoing: std::sync::mpsc::Sender<String>,
}

impl DapClient {
    /// Spawn `program` in `cwd` and frame DAP messages over its stdio.
    /// Returns the client and the channel every incoming message arrives on.
    pub fn start(
        program: &str,
        args: &[String],
        env: &crate::process::EnvOverrides,
        cwd: &Path,
    ) -> Result<(Self, UnboundedReceiver<Value>), String> {
        // Its own process group, so `kill` takes down the debuggee the adapter
        // spawned too, not just the adapter (mirrors `make-process`). A
        // missing cwd is reported as such, not as "not found in PATH".
        let transport::StdioTransport {
            group,
            stdin,
            stdout,
            stderr,
        } = transport::spawn_stdio(program, args, cwd, env).map_err(|e| e.to_string())?;

        let (tx, rx) = unbounded();
        // Forward the adapter's stderr as synthetic DAP `output` events, so
        // Scheme's one console handler shows a crash or startup failure
        // without a second channel (the LSP client's `$/stderr` trick).
        let tx_err = tx.clone();
        std::thread::spawn(move || {
            transport::for_each_stderr_line(stderr, |line| {
                let msg = json!({
                    "seq": 0, "type": "event", "event": "output",
                    "body": { "category": "stderr", "output": format!("{line}\n") }
                });
                tx_err.unbounded_send(msg).is_ok()
            });
        });
        std::thread::spawn(move || transport::read_frames(BufReader::new(stdout), &tx));

        // Writer thread: drains framed messages to stdin off the UI thread.
        let client = Self {
            child: Some(group),
            socket: None,
            outgoing: transport::spawn_writer(stdin),
        };
        Ok((client, rx))
    }

    /// Connect to an adapter serving DAP on a socket instead of stdio (delve's
    /// `dlv dap`, js-debug's `dapDebugServer`). Same framing, same channel.
    ///
    /// Never blocks the caller: name resolution and the connect (bounded by
    /// [`CONNECT_TIMEOUT`]) run on the writer thread, and messages sent
    /// meanwhile queue up in order and go out once the socket is up — so
    /// Scheme can send `initialize` right after `dap-connect`. A failed
    /// connect closes the message channel; [`Self::finish`] reports why.
    pub fn connect(host: &str, port: u16) -> (Self, UnboundedReceiver<Value>) {
        let (tx, rx) = unbounded();
        let (outgoing, out_rx) = channel::<String>();
        let slot = Arc::new(Mutex::new(SocketSlot::default()));
        let shared = slot.clone();
        let host = host.to_string();
        std::thread::spawn(move || {
            // `tx` is moved in here: on any early return it drops, closing
            // the message channel, which is how the editor learns of failure.
            let fail = |why: String| shared.lock().unwrap().error = Some(why);
            let addrs = match (host.as_str(), port).to_socket_addrs() {
                Ok(addrs) => addrs.collect::<Vec<_>>(),
                Err(e) => return fail(format!("resolving {host}: {e}")),
            };
            let mut last_err = format!("{host}:{port}: no addresses");
            let mut stream = None;
            for addr in addrs {
                match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) => last_err = format!("connecting to {host}:{port}: {e}"),
                }
            }
            let Some(writer) = stream else {
                return fail(last_err);
            };
            let (reader, shutdown) = match (writer.try_clone(), writer.try_clone()) {
                (Ok(r), Ok(s)) => (r, s),
                (Err(e), _) | (_, Err(e)) => return fail(format!("cloning socket: {e}")),
            };
            {
                let mut slot = shared.lock().unwrap();
                if slot.cancelled {
                    let _ = writer.shutdown(std::net::Shutdown::Both);
                    return;
                }
                slot.stream = Some(shutdown);
            }
            std::thread::spawn(move || transport::read_frames(BufReader::new(reader), &tx));
            transport::write_frames(out_rx, writer);
        });

        (
            Self {
                child: None,
                socket: Some(slot),
                outgoing,
            },
            rx,
        )
    }

    /// Frame and queue one complete DAP message (already-serialized JSON).
    /// Non-blocking: the writer thread owns the pipe. If the channel is gone
    /// the adapter has died; the editor drops the session when its message
    /// channel closes.
    pub fn send(&self, body: &str) {
        let _ = self.outgoing.send(transport::frame(body));
    }

    /// End the session: kill the adapter's process group (stdio) or shut the
    /// socket down / cancel the pending connect (TCP). The reader thread then
    /// sees EOF and the editor's message pump reports the exit.
    pub fn kill(&mut self) {
        if let Some(group) = self.child.as_mut() {
            group.kill_now(); // a no-op once reaped
        }
        if let Some(slot) = self.socket.as_ref() {
            let mut slot = slot.lock().unwrap();
            slot.cancelled = true;
            if let Some(stream) = slot.stream.as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    /// Reap the session after its message channel has closed and return
    /// `(exit code, failure reason)`. BLOCKING — it may wait up to
    /// [`REAP_GRACE`] for an adapter that closed its stdio but lingers, then
    /// SIGKILLs its group — so run it off the UI thread. A TCP session has no
    /// child: 0, or -1 with the reason when the connect itself failed.
    pub fn finish(mut self) -> (i32, Option<String>) {
        if let Some(slot) = self.socket.as_ref() {
            let error = slot.lock().unwrap().error.take();
            return (if error.is_some() { -1 } else { 0 }, error);
        }
        let Some(group) = self.child.as_mut() else {
            return (0, None);
        };
        let status = group.reap_with_grace(REAP_GRACE);
        (status.and_then(|s| s.code()).unwrap_or(-1), None)
    }
}

/// A session dropped without an orderly [`DapClient::finish`] (editor torn
/// down, app quitting) must not leave the adapter and its debuggee running.
impl Drop for DapClient {
    fn drop(&mut self) {
        // SIGKILLs the group; dropping the `ProcessGroup` then reaps it.
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    #[test]
    fn start_with_missing_cwd_reports_the_directory() {
        let missing = Path::new("/definitely/not/a/real/dir/macros-dap-test");
        let err = DapClient::start("sh", &[], &Default::default(), missing)
            .err()
            .unwrap();
        assert!(err.contains("working directory"), "{err}");
    }

    fn read_frame<R: BufRead>(reader: &mut R) -> Option<Value> {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().ok()?;
            }
        }
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }

    /// The TCP transport frames an outgoing message and parses a framed reply
    /// — one round trip against a scratch listener standing in for `dlv dap`.
    #[test]
    fn tcp_transport_round_trips_frames() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let request = read_frame(&mut reader).unwrap();
            assert_eq!(request["command"], "initialize");
            let reply = json!({
                "seq": 1, "type": "response",
                "request_seq": request["seq"], "success": true
            });
            let body = reply.to_string();
            sock.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
                .unwrap();
            sock.flush().unwrap();
        });

        // Sent before the connect has necessarily completed: it must queue,
        // not be dropped (dap.scm sends `initialize` right after dap-connect).
        let (mut client, mut rx) = DapClient::connect("127.0.0.1", port);
        client.send(&json!({ "seq": 7, "type": "request", "command": "initialize" }).to_string());
        let message = futures::executor::block_on(rx.next()).expect("no reply frame");
        assert_eq!(message["type"], "response");
        assert_eq!(message["request_seq"], 7);
        server.join().unwrap();
        client.kill();
        assert!(futures::executor::block_on(rx.next()).is_none());
        assert_eq!(client.finish(), (0, None));
    }

    /// A refused connect closes the channel and reports why, instead of
    /// blocking the caller or hanging the session in "starting".
    #[test]
    fn tcp_connect_failure_closes_channel_with_reason() {
        // Port 0 never accepts. (A bound-then-dropped ephemeral port isn't
        // safe: a test running in parallel can be handed it and listen.)
        let (client, mut rx) = DapClient::connect("127.0.0.1", 0);
        assert!(futures::executor::block_on(rx.next()).is_none());
        let (code, why) = client.finish();
        assert_eq!(code, -1);
        assert!(why.unwrap().contains("connecting"), "no failure reason");
    }

    /// The stdio transport: frames go to the adapter's stdin, framed stdout
    /// comes back, stderr arrives as synthetic `output` events, and the exit
    /// code is reaped. A tiny python echo adapter stands in for debugpy.
    #[test]
    fn stdio_transport_round_trips_and_reaps() {
        if Command::new("python3").arg("-V").output().is_err() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let script = r#"
import sys, json
inp = sys.stdin.buffer
n = 0
while True:
    line = inp.readline()
    if not line: sys.exit(3)
    line = line.strip()
    if not line: break
    if line.lower().startswith(b"content-length:"): n = int(line.split(b":")[1])
req = json.loads(inp.read(n))
sys.stderr.write("adapter says hi\n"); sys.stderr.flush()
body = json.dumps({"seq": 1, "type": "response", "request_seq": req["seq"], "success": True}).encode()
sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body); sys.stdout.flush()
sys.exit(5)
"#;
        let (client, mut rx) = DapClient::start(
            "python3",
            &["-c".to_string(), script.to_string()],
            &Default::default(),
            Path::new("."),
        )
        .unwrap();
        client.send(&json!({ "seq": 4, "type": "request", "command": "initialize" }).to_string());
        let mut got_response = false;
        let mut got_stderr = false;
        while let Some(m) = futures::executor::block_on(rx.next()) {
            if m["type"] == "response" {
                assert_eq!(m["request_seq"], 4);
                got_response = true;
            } else if m["event"] == "output" && m["body"]["category"] == "stderr" {
                assert_eq!(m["body"]["output"], "adapter says hi\n");
                got_stderr = true;
            }
        }
        assert!(got_response && got_stderr);
        assert_eq!(client.finish(), (5, None));
    }

    /// End-to-end smoke test against a real delve (`dlv dap`): initialize →
    /// launch → initialized → setBreakpoints → configurationDone → stopped at
    /// the breakpoint → stackTrace → disconnect. Needs `dlv` and `go` on
    /// PATH, and builds a Go program, so it's opt-in:
    /// `cargo test dap -- --ignored`. On macOS it also needs Developer Mode
    /// (`DevToolsSecurity -status`): otherwise debugserver waits on a GUI
    /// authorization prompt after `launch` and the test times out there —
    /// initialize, the build, and launch-error reporting are still exercised.
    #[test]
    #[ignore]
    #[cfg(unix)]
    fn delve_smoke_test() {
        use std::os::unix::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("macros-dap-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // macOS: $TMPDIR is /var/… → /private/var/…; go resolves the module
        // root from the REAL cwd, so a symlinked program path is "outside main
        // module" (the reason dap.scm launches by truename).
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("go.mod"), "module smoke\n\ngo 1.21\n").unwrap();
        let main = dir.join("main.go");
        std::fs::write(
            &main,
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tx := 41\n\tx++\n\tfmt.Println(x)\n}\n",
        )
        .unwrap();
        let main = std::fs::canonicalize(&main).unwrap();

        let mut dlv = Command::new("dlv")
            .args(["dap", "--listen=127.0.0.1:0"])
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("dlv not on PATH");
        // Take dlv (and the debuggee/debugserver it spawned) down even when
        // an assertion panics.
        struct KillGroup(i32);
        impl Drop for KillGroup {
            fn drop(&mut self) {
                // SAFETY: `kill` takes no pointers; signalling a process group
                // that has already exited just returns ESRCH.
                unsafe {
                    libc::kill(-self.0, libc::SIGKILL);
                }
            }
        }
        let _guard = KillGroup(dlv.id() as i32);
        let mut out = BufReader::new(dlv.stdout.take().unwrap());
        let mut line = String::new();
        let port: u16 = loop {
            line.clear();
            assert!(out.read_line(&mut line).unwrap() > 0, "dlv exited early");
            if let Some(addr) = line.split("listening at:").nth(1) {
                break addr.trim().rsplit(':').next().unwrap().parse().unwrap();
            }
        };

        let (mut client, mut async_rx) = DapClient::connect("127.0.0.1", port);
        // Forward into a std channel so every wait can time out instead of
        // hanging the test run.
        let (fwd, mut rx) = channel::<Value>();
        std::thread::spawn(move || {
            while let Some(m) = futures::executor::block_on(async_rx.next()) {
                if fwd.send(m).is_err() {
                    break;
                }
            }
        });
        let mut seq = 0;
        let mut send = |command: &str, arguments: Value| {
            seq += 1;
            client.send(
                &json!({"seq": seq, "type": "request", "command": command, "arguments": arguments})
                    .to_string(),
            );
            seq
        };
        let next = |rx: &mut std::sync::mpsc::Receiver<Value>, pred: &dyn Fn(&Value) -> bool| loop {
            // On macOS with Developer Mode off, launching the debuggee blocks
            // on a GUI authorization prompt for debugserver — the usual cause
            // of a timeout right after `launch` (`DevToolsSecurity -enable`).
            let m = rx
                .recv_timeout(Duration::from_secs(90))
                .expect("timed out / channel closed waiting on delve");
            eprintln!("<- {m}");
            assert_ne!(m["success"], false, "request failed: {m}");
            if pred(&m) {
                return m;
            }
        };

        let init = send(
            "initialize",
            json!({"adapterID": "go", "linesStartAt1": true, "pathFormat": "path"}),
        );
        next(&mut rx, &|m| {
            m["type"] == "response" && m["request_seq"] == init
        });
        send(
            "launch",
            json!({"request": "launch", "mode": "debug", "program": dir}),
        );
        next(&mut rx, &|m| m["event"] == "initialized");
        let bp = send(
            "setBreakpoints",
            json!({"source": {"path": main}, "breakpoints": [{"line": 7}]}),
        );
        let r = next(&mut rx, &|m| {
            m["type"] == "response" && m["request_seq"] == bp
        });
        assert_eq!(r["body"]["breakpoints"][0]["verified"], true, "{r}");
        send("configurationDone", json!({}));
        let stopped = next(&mut rx, &|m| m["event"] == "stopped");
        let tid = stopped["body"]["threadId"].clone();
        let st = send("stackTrace", json!({"threadId": tid, "levels": 1}));
        let r = next(&mut rx, &|m| {
            m["type"] == "response" && m["request_seq"] == st
        });
        let frame = &r["body"]["stackFrames"][0];
        assert_eq!(frame["line"], 7, "{r}");
        assert_eq!(
            std::fs::canonicalize(frame["source"]["path"].as_str().unwrap()).unwrap(),
            main
        );
        let dc = send("disconnect", json!({"terminateDebuggee": true}));
        next(&mut rx, &|m| {
            m["type"] == "response" && m["request_seq"] == dc
        });
        client.kill();
        let _ = dlv.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
