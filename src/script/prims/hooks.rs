//! Hooks, completion-at-point functions and timers.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::HooksTimers);
    register_binary(reg, queue, "add-hook", EditorCommand::AddHook);
    register_binary(reg, queue, "remove-hook", EditorCommand::RemoveHook);
    register_unary(
        reg,
        queue,
        "add-completion-at-point-function",
        EditorCommand::AddCapf,
    );
    register_binary(reg, queue, "run-with-timer", |ms, callback| {
        EditorCommand::RunWithTimer { ms, callback }
    });
}
