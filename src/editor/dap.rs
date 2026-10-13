//! DAP sessions for [`Editor`] (`dap-start` / `dap-connect` / `dap-send` /
//! `dap-kill`), extracted from the editor module for size. These methods
//! extend the same `Editor` type; cross-module entry points are `pub(super)`.

use futures::StreamExt;
use gpui::Context;

use crate::script::DapStartArgs;

use super::*;

impl Editor {
    // --- DAP sessions (`dap-start` / `dap-connect` / `dap-send` / `dap-kill`) ---
    // The [`crate::dap::DapClient`] transport is protocol-free; these methods
    // only pump its framed messages to the Steel callbacks, exactly like
    // [`make_process`](Self::make_process) pumps stdout lines.

    /// Start a stdio DAP session: spawn the adapter and forward every framed
    /// message to Steel fn `on_message` as raw JSON text.
    pub(super) fn dap_start(&mut self, args: DapStartArgs, cx: &mut Context<Self>) {
        let dir = if args.cwd.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
        } else {
            std::path::PathBuf::from(&args.cwd)
        };
        match crate::dap::DapClient::start(&args.program, &args.args, &args.env, &dir) {
            Ok((client, rx)) => {
                self.dap_sessions.insert(args.id, client);
                self.dap_pump(args.id, rx, args.on_message, args.on_exit, cx);
            }
            Err(e) => self.dap_failed(&format!("dap-start: {e}"), &args.on_exit, cx),
        }
    }

    /// Start a TCP DAP session (adapters that serve on a socket, e.g. delve).
    /// The connect itself happens off the UI thread (see
    /// [`DapClient::connect`](crate::dap::DapClient::connect)); messages sent
    /// before it completes are queued, and a failed connect surfaces through
    /// the pump's exit path like an adapter crash.
    pub(super) fn dap_connect(
        &mut self,
        id: u64,
        host: String,
        port: u16,
        on_message: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (client, rx) = crate::dap::DapClient::connect(&host, port);
        self.dap_sessions.insert(id, client);
        self.dap_pump(id, rx, on_message, on_exit, cx);
    }

    /// A session that never came up: echo why and still fire `on_exit` so the
    /// Scheme state machine resets instead of waiting forever.
    fn dap_failed(&mut self, error: &str, on_exit: &Option<String>, cx: &mut Context<Self>) {
        self.echo(error.to_string(), cx);
        if let Some(cb) = on_exit {
            self.script.dispatch_action(cb, Some("-1"));
        }
    }

    /// Drain a session's incoming messages on the foreground, calling the
    /// Steel `on_message` per message; when the channel closes (adapter died,
    /// was killed, or never connected) reap it OFF the UI thread and call
    /// `on_exit` with the exit code. Also kills the adapter if the app quits
    /// mid-session, so no adapter/debuggee outlives the editor.
    fn dap_pump(
        &mut self,
        id: u64,
        mut rx: futures::channel::mpsc::UnboundedReceiver<serde_json::Value>,
        on_message: String,
        on_exit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        cx.on_app_quit(move |editor, _| {
            // Dropping the client kills its process group / socket.
            editor.dap_sessions.remove(&id);
            async {}
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let alive = this
                    .update(cx, |editor, _| {
                        // Publish the active buffer first, so a callback reading
                        // `(current-buffer)` sees where the user actually is
                        // (mirrors `make_process`).
                        editor.publish_point();
                        let text = message.to_string();
                        editor.script.dispatch_action(&on_message, Some(&text));
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            let Ok(Some(client)) = this.update(cx, |editor, _| editor.dap_sessions.remove(&id))
            else {
                return;
            };
            // Reaping can wait on a lingering adapter — never on the UI thread.
            let (code, error) = cx
                .background_executor()
                .spawn(async move { client.finish() })
                .await;
            let _ = this.update(cx, |editor, cx| {
                if let Some(why) = error {
                    editor.echo(format!("dap: {why}"), cx);
                }
                if let Some(cb) = &on_exit {
                    editor.script.dispatch_action(cb, Some(&code.to_string()));
                }
            });
        })
        .detach();
    }

    /// Frame one complete DAP JSON message to a live session's adapter. A
    /// send to a session that already died (or failed to start) is dropped
    /// silently: its `on_exit` already reports why, and an echo here would
    /// overwrite that more useful message.
    pub(super) fn dap_send(&mut self, id: u64, message: &str) {
        if let Some(client) = self.dap_sessions.get(&id) {
            client.send(message);
        }
    }

    /// Kill a live session's adapter; the pump then reports its exit.
    pub(super) fn dap_kill(&mut self, id: u64) {
        if let Some(client) = self.dap_sessions.get_mut(&id) {
            client.kill();
        }
    }
}
