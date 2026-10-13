//! Native spell checking (scheme/spell.scm's 'native backend).

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Spell);
    // --- Native spell checking (scheme/spell.scm's 'native backend) -------
    // `(spell-native-available?)` → #t where the OS provides a checker.
    reg.register_fn("spell-native-available?", crate::spell::available);
    // `(spell-check-text text callback)` — callback gets "start\tend\tword"
    // rows (byte offsets into text), newline-joined; "" when nothing is wrong.
    register_binary(reg, queue, "spell-check-text", |text, callback| {
        EditorCommand::SpellCheck { text, callback }
    });
    // `(spell-suggest word callback)` — callback gets one suggestion per line.
    register_binary(reg, queue, "spell-suggest", |word, callback| {
        EditorCommand::SpellSuggest { word, callback }
    });
    register_unary(
        reg,
        queue,
        "spell-learn-word",
        EditorCommand::SpellLearnWord,
    );
}
