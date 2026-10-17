//! ANVIL REPL primitives: a second, independent Steel interpreter.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Anvil);
    // --- ANVIL REPL (a second, independent Steel interpreter) -------------
    {
        // `(anvil-eval source on-result)` evaluates `source` in the separate
        // REPL image; the printed result is delivered to the Scheme function
        // named `on-result` (in this engine) as a single string argument.
        register_binary(reg, queue, "anvil-eval", |source, on_result| {
            EditorCommand::AnvilEval { source, on_result }
        });
    }
    {
        // `(anvil-interrupt)` cancels the REPL's current evaluation;
        // `(anvil-reset)` rebuilds the REPL image from scratch.
        register_nullary(reg, queue, "anvil-interrupt", EditorCommand::AnvilInterrupt);
        register_nullary(reg, queue, "anvil-reset", EditorCommand::AnvilReset);
        // The REPL buffer itself: open it, RET, and input history (the buffer's
        // input region is tracked in Rust — see editor/anvil_repl.rs).
        register_nullary(reg, queue, "anvil-repl-open", EditorCommand::AnvilReplOpen);
        register_nullary(
            reg,
            queue,
            "anvil-send-input",
            EditorCommand::AnvilSendInput,
        );
        register_nullary(
            reg,
            queue,
            "anvil-history-previous",
            EditorCommand::AnvilHistory(true),
        );
        register_nullary(
            reg,
            queue,
            "anvil-history-next",
            EditorCommand::AnvilHistory(false),
        );
        // SLIME-style interaction commands. The editor computes the buffer
        // context (the form at point, the symbol at point, the file path) since
        // that needs the live buffer; the result streams back to the REPL.
        register_nullary(
            reg,
            queue,
            "anvil-eval-defun",
            EditorCommand::AnvilEvalDefun,
        );
        register_nullary(reg, queue, "anvil-load-file", EditorCommand::AnvilLoadFile);
        register_nullary(reg, queue, "anvil-doc", EditorCommand::AnvilDoc);
        register_nullary(
            reg,
            queue,
            "anvil-macroexpand",
            EditorCommand::AnvilMacroexpand,
        );
        register_unary(reg, queue, "anvil-apropos", EditorCommand::AnvilApropos);
        register_nullary(reg, queue, "anvil-signature", EditorCommand::AnvilSignature);
    }
}
