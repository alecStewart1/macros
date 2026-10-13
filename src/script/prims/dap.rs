//! Debug Adapter Protocol transport primitives.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    let async_ids = &ctx.ids;
    reg.section(Section::Dap);
    // --- DAP (Debug Adapter Protocol) --------------------------------------
    {
        // `(dap-start program args cwd on-message on-exit)` → a session id
        // (used by dap-send / dap-kill). The adapter speaks Content-Length-
        // framed DAP JSON on stdio; every incoming message — response, event,
        // or reverse request — reaches `on-message` as raw JSON text (parse
        // with `string->jsexpr`). The transport is protocol-free: dap.scm
        // stamps `seq` and owns the whole state machine. `cwd` "" = inherit;
        // on-exit "" = none (it receives the exit code as a string).
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "dap-start",
            move |program: String,
                  StringList(args): StringList,
                  cwd: String,
                  on_message: Callback,
                  on_exit: Callback|
                  -> usize {
                let id = ids.next();
                q.push(EditorCommand::DapStart(Box::new(DapStartArgs {
                    id,
                    program,
                    args,
                    env: crate::process::current(),
                    cwd,
                    on_message: on_message.0,
                    on_exit: Some(on_exit.0).filter(|s| !s.is_empty()),
                })));
                id as usize
            },
        );
    }
    {
        // `(dap-connect host port on-message on-exit)` → a session id, for
        // adapters that serve DAP over a socket instead of stdio (delve's
        // `dlv dap`); Scheme spawns the server itself (make-process) and
        // connects once it announces its port.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "dap-connect",
            move |host: String,
                  port: usize,
                  on_message: Callback,
                  on_exit: Callback|
                  -> Result<usize, String> {
                // Reject rather than silently wrap (70000 as u16 = 4464).
                let port = u16::try_from(port)
                    .ok()
                    .filter(|&p| p != 0)
                    .ok_or_else(|| format!("dap-connect: invalid port {port}"))?;
                let id = ids.next();
                q.push(EditorCommand::DapConnect {
                    id,
                    host,
                    port,
                    on_message: on_message.0,
                    on_exit: Some(on_exit.0).filter(|s| !s.is_empty()),
                });
                Ok(id as usize)
            },
        );
    }
    register_binary(reg, queue, "dap-send", |id, message| {
        EditorCommand::DapSend { id, message }
    });
    register_unary(reg, queue, "dap-kill", |id| EditorCommand::DapKill { id });
}
