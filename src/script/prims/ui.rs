//! Mode line and title bar primitives.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Ui);
    register_unary(
        reg,
        queue,
        "set-modeline-project",
        EditorCommand::SetModelineProject,
    );
    register_unary(
        reg,
        queue,
        "set-modeline-git",
        EditorCommand::SetModelineGit,
    );
    // `(set-mode-line-slot key value)` → write/clear a named mode-line register
    // (empty value clears). The general form behind project/git indicators.
    register_binary(
        reg,
        queue,
        "set-mode-line-slot",
        EditorCommand::SetModeLineSlot,
    );
    {
        // `(set-mode-line-format segs)` — replace the mode-line layout. `segs` is
        // a list of segment tokens (bare strings = literals, or `ml-field`/
        // `ml-slot`/`ml-text` tab-encoded tokens). Defined from scheme/modeline.scm.
        let q = queue.clone();
        reg.register_fn(
            "set-mode-line-format",
            move |StringList(segs): StringList| {
                q.push(EditorCommand::SetModeLineFormat(segs));
            },
        );
    }
    {
        // `(set-titlebar-dropdown items active callback)` — publish the
        // title-bar dropdown (the perspectives switcher). `items` is a list of
        // display labels, `active` the one shown in the bar; clicking an item
        // calls `(callback label)`. An empty list removes the control. Driven
        // from scheme/perspectives.scm.
        let q = queue.clone();
        reg.register_fn(
            "set-titlebar-dropdown",
            move |StringList(items): StringList, active: String, callback: Callback| {
                q.push(EditorCommand::SetTitlebarDropdown {
                    items,
                    active,
                    callback: callback.0,
                });
            },
        );
    }
    {
        // `(set-titlebar-menu title labels commands)` — publish a title-bar
        // menu named `title`, right of the perspectives switcher. `labels` and
        // `commands` are parallel lists; clicking a row calls its command (a
        // function name) with no arguments. An empty list removes the menu.
        // Driven from scheme/titlebar-menu.scm.
        let q = queue.clone();
        reg.register_fn(
            "set-titlebar-menu",
            move |title: String,
                  StringList(items): StringList,
                  StringList(commands): StringList| {
                q.push(EditorCommand::SetTitlebarMenu {
                    title,
                    items,
                    commands,
                });
            },
        );
    }
}
