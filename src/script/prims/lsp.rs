//! Language-server primitives: server configuration, requests, edits and
//! diagnostics.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared {
        queue, snapshot, ..
    } = ctx;
    reg.section(Section::Lsp);
    {
        // Captures the `with-process-env` overrides now: the server is spawned
        // later, on the editor thread, outside this extent.
        let q = queue.clone();
        reg.register_fn("set-lsp", move |language: String, command: String| {
            q.push(EditorCommand::SetLsp(
                language,
                command,
                crate::process::current(),
            ));
        });
    }
    register_unary(reg, queue, "lsp-rename-to", EditorCommand::LspRename);
    register_unary(
        reg,
        queue,
        "lsp-workspace-symbol-go",
        EditorCommand::LspWorkspaceSymbol,
    );
    {
        let q = queue.clone();
        // (lsp-request method kind params callback) — the generic bridge.
        reg.register_fn(
            "lsp-request",
            move |method: String, kind: String, params: String, callback: Callback| {
                let callback = callback.0;
                q.push(EditorCommand::LspRequest {
                    method,
                    kind: crate::lsp::ParamsKind::parse(&kind),
                    params,
                    callback,
                });
            },
        );
    }
    register_ternary(reg, queue, "lsp-goto", |uri, line, character| {
        EditorCommand::LspGoto {
            uri,
            line,
            character,
        }
    });
    register_unary(
        reg,
        queue,
        "lsp-apply-workspace-edit",
        EditorCommand::LspApplyWorkspaceEdit,
    );
    register_unary(
        reg,
        queue,
        "lsp-apply-text-edits",
        EditorCommand::LspApplyTextEdits,
    );
    register_binary(
        reg,
        queue,
        "set-lsp-format-on-save",
        EditorCommand::SetLspFormatOnSave,
    );
    register_unary(
        reg,
        queue,
        "lsp-semantic-tokens",
        EditorCommand::LspSemanticTokens,
    );
    register_unary(
        reg,
        queue,
        "lsp-semantic-tokens-range",
        EditorCommand::LspSemanticTokensRange,
    );
    register_unary(
        reg,
        queue,
        "lsp-semantic-tokens-delta",
        EditorCommand::LspSemanticTokensDelta,
    );
    // `(diagnostics)` → the active buffer's LSP diagnostics, each a record
    // "line\tcol\tseverity\tmessage" (line/col 0-based). The library splits it.
    register_snapshot_read(reg, snapshot, "diagnostics", |s| s.diagnostics.clone());
}
